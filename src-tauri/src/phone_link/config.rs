use serde::Deserialize;
use std::sync::OnceLock;

// The relay is fixed per build; the PWA build reads the same file.
const RAW: &str = include_str!("../../../phone/relay.config.json");

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelayConfig {
    #[serde(rename = "ref")]
    pub project_ref: String,
    pub url: String,
    pub anon_key: String,
    /// Where the QR points (D-025); the fragment carries the pairing offer.
    pub pwa_url: String,
}

impl RelayConfig {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let cfg: RelayConfig = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        if cfg.project_ref.is_empty()
            || !cfg.project_ref.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        {
            return Err("bad project ref".into());
        }
        if cfg.url != format!("https://{}.supabase.co", cfg.project_ref) {
            return Err("url does not match project ref".into());
        }
        if cfg.anon_key.is_empty() || cfg.anon_key.bytes().any(|b| !b.is_ascii_graphic()) {
            return Err("bad anon key".into());
        }
        let pwa = reqwest::Url::parse(&cfg.pwa_url).map_err(|_| "bad pwa url")?;
        if pwa.scheme() != "https" || pwa.fragment().is_some() || pwa.query().is_some() || !cfg.pwa_url.ends_with('/') {
            return Err("bad pwa url".into());
        }
        Ok(cfg)
    }

    pub fn host(&self) -> String {
        format!("{}.supabase.co", self.project_ref)
    }

    pub fn ws_url(&self) -> String {
        format!(
            "wss://{}/realtime/v1/websocket?apikey={}&vsn=1.0.0",
            self.host(),
            self.anon_key
        )
    }

    pub fn rest_url(&self, path: &str) -> String {
        format!("{}{}", self.url, path)
    }
}

pub fn relay() -> &'static RelayConfig {
    static CFG: OnceLock<RelayConfig> = OnceLock::new();
    CFG.get_or_init(|| RelayConfig::parse(RAW).expect("phone/relay.config.json is invalid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_config_is_valid() {
        let cfg = relay();
        assert!(cfg.ws_url().starts_with("wss://"));
        assert!(cfg.ws_url().contains("/realtime/v1/websocket?apikey="));
        assert_eq!(cfg.rest_url("/auth/v1/settings"), format!("https://{}/auth/v1/settings", cfg.host()));
    }

    #[test]
    fn rejects_mismatched_url_and_unknown_fields() {
        let bad_url = r#"{"ref":"abc","url":"https://evil.example","anonKey":"k"}"#;
        assert!(RelayConfig::parse(bad_url).is_err());
        let extra = r#"{"ref":"abc","url":"https://abc.supabase.co","anonKey":"k","pwaUrl":"https://x.github.io/p/","x":1}"#;
        assert!(RelayConfig::parse(extra).is_err());
        let bad_ref = r#"{"ref":"a.b","url":"https://a.b.supabase.co","anonKey":"k"}"#;
        assert!(RelayConfig::parse(bad_ref).is_err());
        let ok = r#"{"ref":"abc","url":"https://abc.supabase.co","anonKey":"k","pwaUrl":"https://x.github.io/p/"}"#;
        assert!(RelayConfig::parse(ok).is_ok());
    }

    #[test]
    fn rejects_bad_key_and_ref_shapes() {
        assert!(RelayConfig::parse(r#"{"ref":"ABC","url":"https://ABC.supabase.co","anonKey":"k"}"#).is_err());
        assert!(RelayConfig::parse(r#"{"ref":"","url":"https://.supabase.co","anonKey":"k"}"#).is_err());
        assert!(RelayConfig::parse(r#"{"ref":"abc","url":"https://abc.supabase.co/","anonKey":"k"}"#).is_err());
        assert!(RelayConfig::parse(r#"{"ref":"abc","url":"http://abc.supabase.co","anonKey":"k"}"#).is_err());
        assert!(RelayConfig::parse(r#"{"ref":"abc","url":"https://abc.supabase.co","anonKey":""}"#).is_err());
        assert!(RelayConfig::parse(r#"{"ref":"abc","url":"https://abc.supabase.co","anonKey":"a b"}"#).is_err());
        assert!(RelayConfig::parse(r#"{"ref":"abc","url":"https://abc.supabase.co"}"#).is_err());
        let pwa = |u: &str| RelayConfig::parse(&format!(r#"{{"ref":"abc","url":"https://abc.supabase.co","anonKey":"k","pwaUrl":"{u}"}}"#));
        assert!(pwa("https://x.github.io/p/").is_ok());
        assert!(pwa("http://x.github.io/p/").is_err());
        assert!(pwa("https://x.github.io/p").is_err());
        assert!(pwa("https://x.github.io/p/#v1.x").is_err());
        assert!(pwa("https://x.github.io/p/?a=1").is_err());
    }

    #[test]
    fn embedded_key_is_not_a_privileged_key() {
        let key = &relay().anon_key;
        if let Some(payload) = key.split('.').nth(1) {
            // Legacy JWT keys carry their role in the payload; it must never be the service role.
            assert!(!payload.starts_with("eyJyb2xlIjoic2VydmljZV9yb2xl"), "service role key in build config");
        }
        assert!(!key.contains("secret"), "secret key in build config");
        assert!(key.starts_with(concat!("sb_", "publishable_")) || key.starts_with("eyJ"));
    }

    #[test]
    fn migration_and_functions_match_the_build_config() {
        let migration = include_str!("../../../supabase/migrations/20261108025235_phone_link_relay.sql");
        let hardening = include_str!("../../../supabase/migrations/20261108030109_phone_link_relay_hardening.sql");
        assert!(
            hardening.contains(&format!("{}/functions/v1/link-sweep", relay().url)),
            "pg_cron job must call this relay"
        );
        // Topic and fid formats must agree between SQL checks and the Edge Function validator.
        let upload = include_str!("../../../supabase/functions/link-upload/paths.ts");
        assert!(migration.contains("^nl-[0-9a-f]{32}$") && upload.contains("^nl-[0-9a-f]{32}$"));
        assert!(hardening.contains("'^nl-[0-9a-f]{32}$'") && hardening.contains("'^[0-9a-f]{32}$'"));
        assert!(upload.contains("/^[0-9a-f]{32}$/"));
        assert!(hardening.contains("revoke execute on function public.link_owner_claim(bytea) from authenticated"));
        let wrappers = include_str!("../../../supabase/migrations/20261108032830_phone_link_invoker_wrappers.sql");
        assert!(!wrappers.contains("grant execute on function link_private.owner_claim"), "the claim stays closed");
        assert_eq!(wrappers.matches("language sql security invoker").count(), 7);
        let owner_check = include_str!("../../../supabase/migrations/20261108041212_phone_link_owner_check.sql");
        assert!(owner_check.contains("function public.link_owner_check()") && !owner_check.contains("delete "), "owner check has no side effects");
        for sql in [migration, hardening] {
            assert!(!sql.contains(concat!("service", "_role_key")) && !upload.contains("eyJ"));
        }
    }
}
