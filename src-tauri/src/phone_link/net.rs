// Outbound transport to the relay (design section 2.3): Windows proxy discovery
// (static, PAC or WPAD), then either a direct TCP connection or an HTTP CONNECT tunnel.
// The WinHTTP WebSocket path for authenticating proxies is added only if the office
// network run of the spike needs it.

use std::fmt;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_PROXY_HEADER: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyAddr {
    pub host: String,
    pub port: u16,
}

impl fmt::Display for ProxyAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Direct,
    Proxy(ProxyAddr),
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Route::Direct => write!(f, "direct"),
            Route::Proxy(p) => write!(f, "proxy {p}"),
        }
    }
}

/// How the route was found, for diagnostics only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxySource {
    None,
    Static,
    Pac(String),
    AutoDetect,
    /// Machine-wide WinHTTP proxy (`netsh winhttp set proxy`, GPO), used when the user has none.
    WinHttpDefault,
}

#[derive(Debug)]
pub enum NetError {
    Connect(String),
    ProxyAuthRequired,
    ProxyRefused(u16),
    ProxyProtocol(String),
    WebSocket(String),
    Timeout,
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetError::Connect(e) => write!(f, "connect failed: {e}"),
            NetError::ProxyAuthRequired => write!(f, "proxy requires authentication (407)"),
            NetError::ProxyRefused(c) => write!(f, "proxy refused CONNECT with HTTP {c}"),
            NetError::ProxyProtocol(e) => write!(f, "bad proxy response: {e}"),
            NetError::WebSocket(e) => write!(f, "websocket failed: {e}"),
            NetError::Timeout => write!(f, "timed out"),
        }
    }
}

impl NetError {
    /// True when the proxy itself is the obstacle (state `blocked_proxy`).
    pub fn is_proxy_block(&self) -> bool {
        matches!(self, NetError::ProxyAuthRequired | NetError::ProxyRefused(_))
    }
}

/// Picks the proxy for HTTPS from a WinINet/WinHTTP proxy list such as
/// `host:8080`, `http=a:80;https=b:443` or `http://host:3128`.
pub fn parse_proxy_list(list: &str) -> Option<ProxyAddr> {
    let mut generic = None;
    for entry in list.split(|c: char| c == ';' || c.is_whitespace()).filter(|e| !e.is_empty()) {
        let (scheme, addr) = match entry.split_once('=') {
            Some((s, a)) => (Some(s.trim().to_ascii_lowercase()), a.trim()),
            None => (None, entry.trim()),
        };
        match scheme.as_deref() {
            Some("https") => return parse_host_port(addr),
            None if generic.is_none() => generic = parse_host_port(addr),
            _ => {}
        }
    }
    generic
}

fn parse_host_port(addr: &str) -> Option<ProxyAddr> {
    let lower = addr.to_ascii_lowercase();
    if lower.starts_with("socks") {
        return None;
    }
    let addr = addr
        .strip_prefix("http://")
        .or_else(|| addr.strip_prefix("HTTP://"))
        .unwrap_or(addr)
        .trim_end_matches('/');
    if addr.is_empty() || addr.contains("://") {
        return None;
    }
    let (host, port) = if let Some(rest) = addr.strip_prefix('[') {
        let (h, after) = rest.split_once(']')?;
        match after.strip_prefix(':') {
            Some(p) => (h, p.parse::<u16>().ok()?),
            None if after.is_empty() => (h, 80),
            None => return None,
        }
    } else if let Some((h, p)) = addr.rsplit_once(':') {
        (h, p.parse::<u16>().ok()?)
    } else {
        (addr, 80)
    };
    if host.is_empty() || port == 0 {
        return None;
    }
    Some(ProxyAddr { host: host.to_string(), port })
}

/// WinINet bypass list check (`<local>`, exact hosts, `*` anywhere: `*.corp`, `10.*`, `*supabase*`).
pub fn bypassed(host: &str, bypass: &str) -> bool {
    let host = host.to_ascii_lowercase();
    bypass
        .split(|c: char| c == ';' || c.is_whitespace())
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .any(|e| {
            if e == "<local>" {
                !host.contains('.')
            } else {
                wildcard_match(e.as_bytes(), host.as_bytes())
            }
        })
}

fn wildcard_match(pat: &[u8], s: &[u8]) -> bool {
    let (mut p, mut i) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while i < s.len() {
        if p < pat.len() && pat[p] == b'*' {
            star = Some((p, i));
            p += 1;
        } else if p < pat.len() && pat[p] == s[i] {
            p += 1;
            i += 1;
        } else if let Some((sp, si)) = star {
            p = sp + 1;
            i = si + 1;
            star = Some((sp, si + 1));
        } else {
            return false;
        }
    }
    pat[p..].iter().all(|&c| c == b'*')
}

/// Parses the proxy's reply to CONNECT. Ok(()) only for a 2xx status.
pub fn parse_connect_response(head: &[u8]) -> Result<(), NetError> {
    let text = std::str::from_utf8(head).map_err(|_| NetError::ProxyProtocol("not utf-8".into()))?;
    let line = text.lines().next().unwrap_or("");
    let mut parts = line.split_whitespace();
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/1.") {
        return Err(NetError::ProxyProtocol(format!("status line {line:?}")));
    }
    let code: u16 = parts
        .next()
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| NetError::ProxyProtocol(format!("status line {line:?}")))?;
    match code {
        200..=299 => Ok(()),
        407 => Err(NetError::ProxyAuthRequired),
        c => Err(NetError::ProxyRefused(c)),
    }
}

/// Opens a tunnel to `host:port` through an HTTP proxy without authentication.
pub async fn connect_tunnel(proxy: &ProxyAddr, host: &str, port: u16) -> Result<TcpStream, NetError> {
    let fut = async {
        let mut stream = TcpStream::connect((proxy.host.as_str(), proxy.port))
            .await
            .map_err(|e| NetError::Connect(format!("proxy {proxy}: {e}")))?;
        let req = format!(
            "CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\nUser-Agent: Nectar\r\nProxy-Connection: Keep-Alive\r\n\r\n"
        );
        stream
            .write_all(req.as_bytes())
            .await
            .map_err(|e| NetError::Connect(e.to_string()))?;
        let mut head = Vec::with_capacity(512);
        let mut buf = [0u8; 512];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            if head.len() > MAX_PROXY_HEADER {
                return Err(NetError::ProxyProtocol("header too long".into()));
            }
            let n = stream.read(&mut buf).await.map_err(|e| NetError::Connect(e.to_string()))?;
            if n == 0 {
                return Err(NetError::ProxyProtocol("proxy closed the connection".into()));
            }
            head.extend_from_slice(&buf[..n]);
        }
        parse_connect_response(&head)?;
        Ok(stream)
    };
    tokio::time::timeout(CONNECT_TIMEOUT, fut).await.map_err(|_| NetError::Timeout)?
}

/// TCP stream to `host:port` over the given route.
pub async fn open_tcp(route: &Route, host: &str, port: u16) -> Result<TcpStream, NetError> {
    match route {
        Route::Direct => tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
            .await
            .map_err(|_| NetError::Timeout)?
            .map_err(|e| NetError::Connect(e.to_string())),
        Route::Proxy(p) => connect_tunnel(p, host, port).await,
    }
}

/// WebSocket over TLS (SChannel through native-tls, so a company root CA is honoured).
pub async fn open_ws(route: &Route, url: &str, host: &str) -> Result<WsStream, NetError> {
    let tcp = open_tcp(route, host, 443).await?;
    let fut = tokio_tungstenite::client_async_tls(url, tcp);
    let (ws, _) = tokio::time::timeout(CONNECT_TIMEOUT, fut)
        .await
        .map_err(|_| NetError::Timeout)?
        .map_err(|e| NetError::WebSocket(e.to_string()))?;
    Ok(ws)
}

/// reqwest client that uses exactly this route (reqwest itself does not read PAC).
pub fn http_client(route: &Route) -> Result<reqwest::Client, String> {
    let builder = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(Duration::from_secs(30))
        .user_agent("Nectar");
    let builder = match route {
        Route::Direct => builder.no_proxy(),
        Route::Proxy(p) => builder.proxy(reqwest::Proxy::all(format!("http://{p}")).map_err(|e| e.to_string())?),
    };
    builder.build().map_err(|e| e.to_string())
}

/// Asks Windows which proxy the current user would use for `url`.
pub fn discover_proxy(url: &str) -> (Route, ProxySource) {
    match winhttp::discover(url) {
        Ok(found) => found,
        Err(e) => {
            crate::diagnostics::log(&format!("phone link: proxy discovery failed: {e}"));
            (Route::Direct, ProxySource::None)
        }
    }
}

mod winhttp {
    use super::{bypassed, parse_proxy_list, ProxySource, Route};
    use windows::core::{BOOL, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{GlobalFree, HGLOBAL};
    use windows::Win32::Networking::WinHttp::*;

    fn take(p: PWSTR) -> Option<String> {
        if p.is_null() {
            return None;
        }
        let s = unsafe { p.to_string() }.ok();
        unsafe {
            let _ = GlobalFree(Some(HGLOBAL(p.0 as _)));
        }
        s.filter(|s| !s.trim().is_empty())
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn host_of(url: &str) -> String {
        let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        authority.rsplit_once(':').map(|(h, _)| h).unwrap_or(authority).to_string()
    }

    /// What the PAC script or WPAD said. Windows uses the static proxy only when the
    /// script is unavailable, never when it answers DIRECT.
    pub(super) enum AutoProxy {
        Proxy(String, Option<String>),
        Direct,
        Unavailable,
    }

    fn auto_proxy(url: &str, pac: Option<&str>) -> Result<AutoProxy, String> {
        let agent = wide("Nectar");
        let session = unsafe { WinHttpOpen(PCWSTR(agent.as_ptr()), WINHTTP_ACCESS_TYPE_NO_PROXY, PCWSTR::null(), PCWSTR::null(), 0) };
        if session.is_null() {
            return Err("WinHttpOpen failed".into());
        }
        let pac_w = pac.map(wide);
        let mut opts = WINHTTP_AUTOPROXY_OPTIONS {
            fAutoLogonIfChallenged: BOOL(1),
            ..Default::default()
        };
        match &pac_w {
            Some(w) => {
                opts.dwFlags = WINHTTP_AUTOPROXY_CONFIG_URL;
                opts.lpszAutoConfigUrl = PCWSTR(w.as_ptr());
            }
            None => {
                opts.dwFlags = WINHTTP_AUTOPROXY_AUTO_DETECT;
                opts.dwAutoDetectFlags = WINHTTP_AUTO_DETECT_TYPE_DHCP | WINHTTP_AUTO_DETECT_TYPE_DNS_A;
            }
        }
        let url_w = wide(url);
        let mut info = WINHTTP_PROXY_INFO::default();
        let res = unsafe { WinHttpGetProxyForUrl(session, PCWSTR(url_w.as_ptr()), &mut opts, &mut info) };
        unsafe {
            let _ = WinHttpCloseHandle(session);
        }
        match res {
            Ok(()) => {
                let named = info.dwAccessType == WINHTTP_ACCESS_TYPE_NAMED_PROXY;
                let proxy = take(info.lpszProxy);
                let bypass = take(info.lpszProxyBypass);
                Ok(match (named, proxy) {
                    (true, Some(list)) => AutoProxy::Proxy(list, bypass),
                    _ => AutoProxy::Direct,
                })
            }
            // No PAC/WPAD found or the script failed: fall back to the static setting.
            Err(e) => {
                crate::diagnostics::log(&format!("phone link: WinHttpGetProxyForUrl: {e}"));
                Ok(AutoProxy::Unavailable)
            }
        }
    }

    pub(super) fn resolve(
        host: &str,
        auto: Option<(AutoProxy, ProxySource)>,
        static_proxy: Option<&str>,
        static_bypass: Option<&str>,
    ) -> (Route, ProxySource) {
        match auto {
            Some((AutoProxy::Proxy(list, bypass), source)) => {
                let skip = bypass.as_deref().map(|b| bypassed(host, b)).unwrap_or(false);
                return match (skip, parse_proxy_list(&list)) {
                    (false, Some(p)) => (Route::Proxy(p), source),
                    _ => (Route::Direct, source),
                };
            }
            Some((AutoProxy::Direct, source)) => return (Route::Direct, source),
            Some((AutoProxy::Unavailable, _)) | None => {}
        }
        if let Some(list) = static_proxy {
            let skip = static_bypass.map(|b| bypassed(host, b)).unwrap_or(false);
            if let (false, Some(p)) = (skip, parse_proxy_list(list)) {
                return (Route::Proxy(p), ProxySource::Static);
            }
        }
        (Route::Direct, ProxySource::None)
    }

    pub fn discover(url: &str) -> Result<(Route, ProxySource), String> {
        let mut cfg = WINHTTP_CURRENT_USER_IE_PROXY_CONFIG::default();
        unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut cfg) }.map_err(|e| e.to_string())?;
        let auto_detect = cfg.fAutoDetect.as_bool();
        let pac = take(cfg.lpszAutoConfigUrl);
        let static_proxy = take(cfg.lpszProxy);
        let static_bypass = take(cfg.lpszProxyBypass);
        let host = host_of(url);

        let auto = if pac.is_some() || auto_detect {
            let found = auto_proxy(url, pac.as_deref())?;
            Some((found, pac.map(ProxySource::Pac).unwrap_or(ProxySource::AutoDetect)))
        } else {
            None
        };
        let found = resolve(&host, auto, static_proxy.as_deref(), static_bypass.as_deref());
        if found.1 == ProxySource::None {
            if let Some(route) = default_proxy(&host) {
                return Ok((route, ProxySource::WinHttpDefault));
            }
        }
        Ok(found)
    }

    /// `WinHttpGetDefaultProxyConfiguration` (I-111): the machine-wide proxy.
    fn default_proxy(host: &str) -> Option<Route> {
        let mut info = WINHTTP_PROXY_INFO::default();
        unsafe { WinHttpGetDefaultProxyConfiguration(&mut info) }.ok()?;
        let named = info.dwAccessType == WINHTTP_ACCESS_TYPE_NAMED_PROXY;
        let proxy = take(info.lpszProxy);
        let bypass = take(info.lpszProxyBypass);
        default_route(host, named, proxy.as_deref(), bypass.as_deref())
    }

    pub(super) fn default_route(host: &str, named: bool, proxy: Option<&str>, bypass: Option<&str>) -> Option<Route> {
        if !named || bypass.is_some_and(|b| bypassed(host, b)) {
            return None;
        }
        parse_proxy_list(proxy?).map(Route::Proxy)
    }

    #[cfg(test)]
    mod tests {
        use super::{resolve, AutoProxy, ProxySource, Route};

        #[test]
        fn machine_wide_proxy_fallback() {
            let h = "abc.supabase.co";
            assert_eq!(super::default_route(h, true, Some("corp:8080"), None).unwrap().to_string(), "proxy corp:8080");
            assert!(super::default_route(h, false, Some("corp:8080"), None).is_none());
            assert!(super::default_route(h, true, Some("corp:8080"), Some("*.supabase.co")).is_none());
            assert!(super::default_route(h, true, None, None).is_none());
        }

        #[test]
        fn host_of_strips_scheme_port_and_path() {
            assert_eq!(super::host_of("https://abc.supabase.co/"), "abc.supabase.co");
            assert_eq!(super::host_of("https://abc.supabase.co:443/x?y"), "abc.supabase.co");
        }

        #[test]
        fn pac_direct_wins_over_static_proxy() {
            let h = "abc.supabase.co";
            let pac = || ProxySource::Pac("http://wpad/pac".into());
            let (r, s) = resolve(h, Some((AutoProxy::Direct, pac())), Some("static:8080"), None);
            assert_eq!((r, s), (Route::Direct, pac()));
            let (r, s) = resolve(h, Some((AutoProxy::Unavailable, pac())), Some("static:8080"), None);
            assert_eq!(r.to_string(), "proxy static:8080");
            assert_eq!(s, ProxySource::Static);
            let (r, _) = resolve(h, Some((AutoProxy::Proxy("pac:3128".into(), None), pac())), Some("static:8080"), None);
            assert_eq!(r.to_string(), "proxy pac:3128");
            let (r, _) = resolve(h, Some((AutoProxy::Proxy("pac:3128".into(), Some("*supabase*".into())), pac())), None, None);
            assert_eq!(r, Route::Direct);
            assert_eq!(resolve(h, None, Some("static:8080"), Some("*.supabase.co")).0, Route::Direct);
            assert_eq!(resolve(h, None, None, None), (Route::Direct, ProxySource::None));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    fn p(host: &str, port: u16) -> Option<ProxyAddr> {
        Some(ProxyAddr { host: host.into(), port })
    }

    #[test]
    fn proxy_list_parsing() {
        assert_eq!(parse_proxy_list("proxy.corp:8080"), p("proxy.corp", 8080));
        assert_eq!(parse_proxy_list("http://proxy.corp:3128/"), p("proxy.corp", 3128));
        assert_eq!(parse_proxy_list("proxy.corp"), p("proxy.corp", 80));
        assert_eq!(parse_proxy_list("http=a:80;https=b:443"), p("b", 443));
        assert_eq!(parse_proxy_list("http=a:80;ftp=c:21"), None);
        assert_eq!(parse_proxy_list("socks=s:1080"), None);
        assert_eq!(parse_proxy_list("PROXY_A:8080; PROXY_B:8081"), p("PROXY_A", 8080));
        assert_eq!(parse_proxy_list("[::1]:8080"), p("::1", 8080));
        assert_eq!(parse_proxy_list(""), None);
        assert_eq!(parse_proxy_list("host:notaport"), None);
    }

    #[test]
    fn bypass_rules() {
        assert!(bypassed("intranet", "<local>"));
        assert!(!bypassed("abc.supabase.co", "<local>"));
        assert!(bypassed("abc.supabase.co", "*.supabase.co;<local>"));
        assert!(bypassed("ABC.supabase.co", "abc.supabase.co"));
        assert!(!bypassed("abc.supabase.co", "*.corp.example"));
        assert!(bypassed("10.1.2.3", "10.*"));
        assert!(!bypassed("110.1.2.3", "10.*"));
        assert!(bypassed("abc.supabase.co", "*supabase*"));
        assert!(bypassed("abc.supabase.co", "abc.*.co"));
        assert!(!bypassed("abc.supabase.com", "*.supabase.co"));
    }

    #[test]
    fn connect_response_status() {
        assert!(parse_connect_response(b"HTTP/1.1 200 Connection established\r\n\r\n").is_ok());
        assert!(parse_connect_response(b"HTTP/1.0 200 OK\r\n\r\n").is_ok());
        assert!(matches!(
            parse_connect_response(b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Negotiate\r\n\r\n"),
            Err(NetError::ProxyAuthRequired)
        ));
        assert!(matches!(parse_connect_response(b"HTTP/1.1 403 Forbidden\r\n\r\n"), Err(NetError::ProxyRefused(403))));
        assert!(matches!(parse_connect_response(b"SSH-2.0\r\n\r\n"), Err(NetError::ProxyProtocol(_))));
        assert!(NetError::ProxyRefused(403).is_proxy_block());
        assert!(!NetError::Timeout.is_proxy_block());
    }

    async fn fake_proxy(reply: &'static [u8]) -> (ProxyAddr, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            let mut buf = [0u8; 256];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = s.read(&mut buf).await.unwrap();
                head.extend_from_slice(&buf[..n]);
            }
            s.write_all(reply).await.unwrap();
            // Echo one message so the caller can check the tunnel carries data.
            let n = s.read(&mut buf).await.unwrap_or(0);
            let _ = s.write_all(&buf[..n]).await;
            String::from_utf8(head).unwrap()
        });
        (ProxyAddr { host: "127.0.0.1".into(), port }, task)
    }

    #[tokio::test]
    async fn tunnel_through_accepting_proxy() {
        let (proxy, task) = fake_proxy(b"HTTP/1.1 200 Connection established\r\n\r\n").await;
        let mut s = connect_tunnel(&proxy, "abc.supabase.co", 443).await.unwrap();
        s.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
        let head = task.await.unwrap();
        assert!(head.starts_with("CONNECT abc.supabase.co:443 HTTP/1.1\r\n"));
        assert!(head.contains("Host: abc.supabase.co:443\r\n"));
    }

    #[tokio::test]
    async fn tunnel_reports_proxy_auth() {
        let (proxy, _task) = fake_proxy(b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: NTLM\r\n\r\n").await;
        let err = connect_tunnel(&proxy, "abc.supabase.co", 443).await.unwrap_err();
        assert!(matches!(err, NetError::ProxyAuthRequired));
    }

    #[test]
    fn discovery_does_not_panic() {
        let (route, _source) = discover_proxy("https://example.supabase.co/");
        let _ = route.to_string();
    }

    #[tokio::test]
    async fn tunnel_reports_refusal_and_early_close() {
        let (proxy, _t) = fake_proxy(b"HTTP/1.1 502 Bad Gateway\x0d\x0a\x0d\x0a").await;
        let err = connect_tunnel(&proxy, "abc.supabase.co", 443).await.unwrap_err();
        assert!(matches!(err, NetError::ProxyRefused(502)));
        assert!(err.is_proxy_block());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 256];
            let _ = s.read(&mut buf).await;
        });
        let closed = ProxyAddr { host: "127.0.0.1".into(), port };
        let err = connect_tunnel(&closed, "abc.supabase.co", 443).await.unwrap_err();
        // A proxy that closes at once surfaces as a protocol error, or as a write error when the
        // close wins the race with our CONNECT (seen under load on Windows).
        assert!(matches!(err, NetError::ProxyProtocol(_) | NetError::Connect(_)), "{err:?}");
        assert!(!err.is_proxy_block());
    }

    #[tokio::test]
    async fn tunnel_rejects_endless_header() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 256];
            let _ = s.read(&mut buf).await;
            let junk = [b'a'; 1024];
            for _ in 0..32 {
                if s.write_all(&junk).await.is_err() {
                    break;
                }
            }
        });
        let proxy = ProxyAddr { host: "127.0.0.1".into(), port };
        let err = connect_tunnel(&proxy, "abc.supabase.co", 443).await.unwrap_err();
        assert!(matches!(err, NetError::ProxyProtocol(_)));
    }

    #[tokio::test]
    async fn tunnel_to_dead_proxy_is_a_connect_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let proxy = ProxyAddr { host: "127.0.0.1".into(), port };
        let err = connect_tunnel(&proxy, "abc.supabase.co", 443).await.unwrap_err();
        assert!(matches!(err, NetError::Connect(_)));
    }

    #[test]
    fn proxy_list_edge_cases() {
        assert_eq!(parse_proxy_list("  proxy.corp:8080  "), p("proxy.corp", 8080));
        assert_eq!(parse_proxy_list("proxy.corp:0"), None);
        assert_eq!(parse_proxy_list("proxy.corp:99999"), None);
        assert_eq!(parse_proxy_list(":8080"), None);
        assert_eq!(parse_proxy_list("https://proxy.corp:443"), None);
        assert_eq!(parse_proxy_list("socks=s:1080;https=b:443"), p("b", 443));
        assert_eq!(parse_proxy_list("[::1]"), p("::1", 80));
        assert_eq!(parse_proxy_list("[::1]x"), None);
    }

    #[test]
    fn bypass_edge_cases() {
        assert!(!bypassed("abc.supabase.co", ""));
        assert!(bypassed("localhost", "<local>"));
        assert!(!bypassed("abc.supabase.co", "other.supabase.co;*.corp"));
        assert!(bypassed("abc.supabase.co", "x.y  abc.supabase.co"));
    }
}
