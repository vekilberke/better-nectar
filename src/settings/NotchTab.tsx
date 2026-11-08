import { AgentSounds } from "./AgentSounds";
import { NotchNotifications } from "./NotchNotifications";
import { NotchDownloads } from "./NotchDownloads";
import { NotchEdgeDelay } from "./NotchEdgeDelay";
import { NetWidgetMode } from "./NetWidgetMode";
import { NotchHotkey } from "./NotchHotkey";
import { MicCamSettings } from "./MicCamSettings";
import {
  PanelTop,
  ClipboardList,
  Calendar,
  Music,
  Pin,
  Sparkle,
  Bot,
  Gauge,
  Inbox,
  History,
  ListOrdered,
  SlidersHorizontal,
  Minimize2,
  LayoutList,
  Palette,
  Sparkles,
  CloudSun,
  X,
  Monitor,
  MonitorCheck,
  CloudCog,
  ChevronRight,
  Hourglass,
  Timer,
  BellRing,
  Globe,
  Volume2,
  Cat,
  MousePointer2,
} from "lucide-react";
import { DEFAULT_PANEL_ORDER, type CcLayout, type PanelId } from "../panelOrder";
import { ControlCenterConfig } from "../components/ControlCenterConfig";
import { OrderList } from "./OrderList";
import { panelIcon } from "../panelIcons";
import { SettingRow } from "./SettingRow";
import { SettingSection } from "./SettingSection";
import { Dropdown } from "./Dropdown";
import { useCaldav } from "../hooks/useCaldav";
import { StatusWidgetConfig } from "../components/StatusWidgetConfig";
import { DualClockPicker } from "../components/DualClockPicker";
import { zoneOffsetLabel } from "../dualClock";
import type { WidgetConfig, MonitorInfo, MonitorMode } from "./types";
import { usePerMonitor } from "./usePerMonitor";
import { useT } from "../i18n";
import { CAT_PALETTE_NAMES } from "../components/sonderCatData";

interface NotchTabProps {
  notchMode: string;
  setNotchModeValue: (mode: string) => void;
  monitors: MonitorInfo[];
  notchMonitorMode: MonitorMode;
  setNotchMonitorModeValue: (mode: MonitorMode) => void;
  notchMonitorId: string;
  setNotchMonitorIdValue: (id: string) => void;
  notchModeByMonitor: Record<string, string>;
  setNotchModeForMonitor: (monitorId: string, mode: string | null) => void;
  replaceNotchModes: (next: Record<string, string>) => void;
  calendarEnabled: boolean;
  toggleCalendar: () => void;
  timerEnabled: boolean;
  toggleTimerEnabled: () => void;
  timerSoundEnabled: boolean;
  toggleTimerSound: () => void;
  notchTimerProgress: boolean;
  toggleNotchTimerProgress: () => void;
  stopwatchEnabled: boolean;
  toggleStopwatchEnabled: () => void;
  legacyMode: boolean;
  dualClockEnabled: boolean;
  toggleDualClock: () => void;
  dualClockZone: string;
  dualClockLabel: string;
  selectDualClockZone: (option: { label: string; zone: string }) => void;
  notchIdle: string;
  changeNotchIdle: (value: string) => void;
  catPalette: string;
  changeCatPalette: (value: string) => void;
  catClaude: boolean;
  toggleCatClaude: () => void;
  musicModeEnabled: boolean;
  toggleMusicMode: () => void;
  musicCompactNotch: boolean;
  toggleMusicCompactNotch: () => void;
  mediaLayout: "classic" | "compact";
  toggleMediaLayout: (layout: "classic" | "compact") => void;
  mediaAmbienceEnabled: boolean;
  toggleAmbience: () => void;
  mediaCompactGlowEnabled: boolean;
  toggleCompactGlow: () => void;
  artworkThemeEnabled: boolean;
  toggleArtworkTheme: () => void;
  agentApi: boolean;
  toggleAgentApi: () => void;
  claudeUsage: boolean;
  toggleClaudeUsage: () => void;
  agentStyle: "mascot" | "bar";
  changeAgentStyle: (style: "mascot" | "bar") => void;
  notchScrollVolume: boolean;
  toggleNotchScrollVolume: () => void;
  shelfPersist: boolean;
  toggleShelfPersist: () => void;
  rememberMode: boolean;
  toggleRememberMode: () => void;
  hoverOpen: boolean;
  toggleHoverOpen: () => void;
  hoverPanel: PanelId;
  changeHoverPanel: (value: string) => void;
  panelControlCenter: boolean;
  togglePanelControlCenter: () => void;
  panelShelf: boolean;
  togglePanelShelf: () => void;
  panelClipboard: boolean;
  togglePanelClipboard: () => void;
  clipboardSize: string;
  changeClipboardSize: (value: string) => void;
  clipboardPersist: boolean;
  toggleClipboardPersist: () => void;
  panelOrder: PanelId[] | null;
  reorderPanels: (order: PanelId[]) => void;
  resetPanelOrder: () => void;
  ccLayout: CcLayout;
  changeCcLayout: (layout: CcLayout) => void;
  resetCcLayout: () => void;
  weatherEnabled: boolean;
  toggleWeather: () => void;
  tempUnitFahrenheit: boolean;
  toggleTempUnit: () => void;
  cityName: string;
  cityQuery: string;
  setCityQuery: (query: string) => void;
  citySearchResults: Array<{ name: string; country: string; latitude: number; longitude: number }>;
  showCityDropdown: boolean;
  setShowCityDropdown: (show: boolean) => void;
  selectCity: (city: { name: string; country: string; latitude: number; longitude: number }) => void;
  handleCityClear: () => void;
  statusWidgets: WidgetConfig;
  handleWidgetsChange: (config: WidgetConfig) => void;
  hasBattery: boolean;
  onOpenCalendarSync: () => void;
}

export function NotchTab({
  notchMode,
  setNotchModeValue,
  monitors,
  notchMonitorMode,
  setNotchMonitorModeValue,
  notchMonitorId,
  setNotchMonitorIdValue,
  notchModeByMonitor,
  setNotchModeForMonitor,
  replaceNotchModes,
  calendarEnabled,
  toggleCalendar,
  timerEnabled,
  toggleTimerEnabled,
  timerSoundEnabled,
  toggleTimerSound,
  notchTimerProgress,
  toggleNotchTimerProgress,
  stopwatchEnabled,
  toggleStopwatchEnabled,
  legacyMode,
  dualClockEnabled,
  toggleDualClock,
  dualClockZone,
  dualClockLabel,
  selectDualClockZone,
  notchIdle,
  changeNotchIdle,
  catPalette,
  changeCatPalette,
  catClaude,
  toggleCatClaude,
  musicModeEnabled,
  toggleMusicMode,
  musicCompactNotch,
  toggleMusicCompactNotch,
  mediaLayout,
  toggleMediaLayout,
  mediaAmbienceEnabled,
  toggleAmbience,
  mediaCompactGlowEnabled,
  toggleCompactGlow,
  artworkThemeEnabled,
  toggleArtworkTheme,
  agentApi,
  toggleAgentApi,
  claudeUsage,
  toggleClaudeUsage,
  agentStyle,
  changeAgentStyle,
  notchScrollVolume,
  toggleNotchScrollVolume,
  shelfPersist,
  toggleShelfPersist,
  rememberMode,
  toggleRememberMode,
  hoverOpen,
  toggleHoverOpen,
  hoverPanel,
  changeHoverPanel,
  panelControlCenter,
  togglePanelControlCenter,
  panelShelf,
  togglePanelShelf,
  panelClipboard,
  togglePanelClipboard,
  clipboardSize,
  changeClipboardSize,
  clipboardPersist,
  toggleClipboardPersist,
  panelOrder,
  reorderPanels,
  resetPanelOrder,
  ccLayout,
  changeCcLayout,
  resetCcLayout,
  weatherEnabled,
  toggleWeather,
  tempUnitFahrenheit,
  toggleTempUnit,
  cityName,
  cityQuery,
  setCityQuery,
  citySearchResults,
  showCityDropdown,
  setShowCityDropdown,
  selectCity,
  handleCityClear,
  statusWidgets,
  handleWidgetsChange,
  hasBattery,
  onOpenCalendarSync,
}: NotchTabProps) {
  const t = useT();
  const caldav = useCaldav();
  const sources = (caldav.account ? 1 : 0) + caldav.links.length;
  const perMonitor = usePerMonitor(notchMode, notchModeByMonitor, replaceNotchModes, monitors);
  const multiMonitor = notchMonitorMode === "all" && monitors.length > 1;
  const showPerMonitor = multiMonitor && perMonitor.enabled;
  return (
    <>
      <SettingSection id="notch-display" title={t("notch.section.display")} defaultOpen>
        {!showPerMonitor && (
          <SettingRow icon={PanelTop} label={t("notch.behavior")} desc={t("notch.behavior.desc")}>
            <Dropdown
              value={notchMode}
              onChange={setNotchModeValue}
              options={[
                { value: "fixed", label: t("common.fixed") },
                { value: "smart", label: t("common.smart") },
                { value: "peek", label: t("common.peek") },
              ]}
            />
          </SettingRow>
        )}

        {notchMode !== "fixed" && <NotchEdgeDelay />}

        <SettingRow icon={MonitorCheck} label={t("common.showOn")} desc={t("notch.showOn.desc")}>
          <Dropdown
            value={notchMonitorMode}
            onChange={(v) => setNotchMonitorModeValue(v as MonitorMode)}
            options={[
              { value: "primary", label: t("common.monitors.primary") },
              { value: "all", label: t("common.monitors.all") },
              { value: "specific", label: t("common.monitors.specific") },
            ]}
          />
        </SettingRow>

        {notchMonitorMode === "specific" && (
          <SettingRow icon={Monitor} label={t("common.monitor")} desc={t("notch.monitor.desc")}>
            <Dropdown
              value={notchMonitorId}
              onChange={setNotchMonitorIdValue}
              placeholder={monitors.length === 0 ? t("common.loading") : undefined}
              options={monitors.map((m) => ({
                value: m.id,
                label: `${m.label}${m.is_primary ? ` (${t("common.primary")})` : ""}`,
              }))}
            />
          </SettingRow>
        )}

        {multiMonitor && (
          <SettingRow icon={Monitor} label={t("common.perMonitor")} desc={t("common.perMonitor.desc")}>
            <label className="toggle-switch">
              <input type="checkbox" checked={perMonitor.enabled} onChange={perMonitor.toggle} />
              <span className="slider"></span>
            </label>
          </SettingRow>
        )}

        {showPerMonitor && monitors.map((m) => (
          <SettingRow key={m.id} icon={Monitor} label={t("common.monitorBehavior", { name: m.label })} desc={m.is_primary ? t("common.primaryMonitor") : undefined}>
            <Dropdown
              value={notchModeByMonitor[m.id] || notchMode}
              onChange={(v) => setNotchModeForMonitor(m.id, v)}
              options={[
                { value: "fixed", label: t("common.fixed") },
                { value: "smart", label: t("common.smart") },
                { value: "peek", label: t("common.peek") },
              ]}
            />
          </SettingRow>
        ))}

      </SettingSection>

      <SettingSection id="notch-calendar" title={t("notch.section.calendar")}>
        <SettingRow icon={Calendar} label={t("notch.calendar")} desc={t("notch.calendar.desc")}>
          <label className="toggle-switch">
            <input type="checkbox" checked={calendarEnabled} onChange={toggleCalendar} />
            <span className="slider"></span>
          </label>
        </SettingRow>

        {calendarEnabled && (
          <SettingRow
            icon={CloudCog}
            label={t("notch.calendarSync")}
            desc={sources ? t(sources === 1 ? "notch.calendarSync.connectedOne" : "notch.calendarSync.connectedMany", { count: sources }) : t("notch.calendarSync.desc")}
            action
            onClick={onOpenCalendarSync}
          >
            <ChevronRight size={14} strokeWidth={1.5} />
          </SettingRow>
        )}

        <SettingRow icon={Hourglass} label={t("notch.timer")} desc={t("notch.timer.desc")}>
          <label className="toggle-switch">
            <input type="checkbox" checked={timerEnabled} onChange={toggleTimerEnabled} />
            <span className="slider"></span>
          </label>
        </SettingRow>

        {timerEnabled && (
          <SettingRow icon={BellRing} label={t("notch.timerSound")} desc={t("notch.timerSound.desc")}>
            <label className="toggle-switch">
              <input type="checkbox" checked={timerSoundEnabled} onChange={toggleTimerSound} />
              <span className="slider"></span>
            </label>
          </SettingRow>
        )}

        {timerEnabled && (
          <SettingRow icon={Timer} label={t("notch.timerProgress")} desc={t("notch.timerProgress.desc")}>
            <label className="toggle-switch">
              <input type="checkbox" checked={notchTimerProgress} onChange={toggleNotchTimerProgress} />
              <span className="slider"></span>
            </label>
          </SettingRow>
        )}

        <SettingRow icon={Timer} label={t("notch.stopwatch")} desc={t("notch.stopwatch.desc")}>
          <label className="toggle-switch">
            <input type="checkbox" checked={stopwatchEnabled} onChange={toggleStopwatchEnabled} />
            <span className="slider"></span>
          </label>
        </SettingRow>

        {!legacyMode && (
          <>
            <SettingRow
              icon={Globe}
              label={t("notch.dualClock")}
              desc={dualClockEnabled ? [dualClockLabel, zoneOffsetLabel(dualClockZone)].filter(Boolean).join(" · ") : t("notch.dualClock.desc")}
            >
              <label className="toggle-switch">
                <input type="checkbox" checked={dualClockEnabled} onChange={toggleDualClock} />
                <span className="slider"></span>
              </label>
            </SettingRow>

            {dualClockEnabled && (
              <>
                <DualClockPicker onSelect={selectDualClockZone} />
                <div className="setting-divider" />
              </>
            )}

            <SettingRow icon={Cat} label={t("notch.idle")} desc={t("notch.idle.desc")}>
              <Dropdown
                value={notchIdle}
                options={[
                  { value: "clock", label: t("notch.idle.clock") },
                  { value: "cat", label: t("notch.idle.cat") },
                  { value: "hybrid", label: t("notch.idle.hybrid") },
                ]}
                onChange={changeNotchIdle}
              />
            </SettingRow>

            {notchIdle !== "clock" && (
              <>
                <SettingRow icon={Palette} label={t("notch.cat.palette")} desc={t("notch.cat.palette.desc")}>
                  <Dropdown
                    value={catPalette}
                    options={CAT_PALETTE_NAMES.map((n) => ({ value: n, label: t("notch.cat.palette." + n.replace(" ", "_")) }))}
                    onChange={changeCatPalette}
                  />
                </SettingRow>
                <SettingRow icon={Sparkle} label={t("notch.cat.claude")} desc={t("notch.cat.claude.desc")}>
                  <label className="toggle-switch">
                    <input type="checkbox" checked={catClaude} onChange={toggleCatClaude} />
                    <span className="slider"></span>
                  </label>
                </SettingRow>
              </>
            )}
          </>
        )}

      </SettingSection>

      <SettingSection id="notch-music" title={t("notch.section.music")}>
        <SettingRow icon={Music} label={t("notch.musicMode")} desc={t("notch.musicMode.desc")}>
          <label className="toggle-switch">
            <input type="checkbox" checked={musicModeEnabled} onChange={toggleMusicMode} />
            <span className="slider"></span>
          </label>
        </SettingRow>

        {musicModeEnabled && (
          <>
            <SettingRow icon={Minimize2} label={t("notch.compactMode")} desc={t("notch.compactMode.desc")}>
              <label className="toggle-switch">
                <input type="checkbox" checked={musicCompactNotch} onChange={toggleMusicCompactNotch} />
                <span className="slider"></span>
              </label>
            </SettingRow>

            <SettingRow icon={LayoutList} label={t("notch.mediaLayout")} desc={t("notch.mediaLayout.desc")}>
              <div className="unit-toggle-minimal wide">
                <span className={mediaLayout === "classic" ? "active" : ""} onClick={() => toggleMediaLayout("classic")}>
                  {t("notch.mediaLayout.classic")}
                </span>
                <span className={mediaLayout === "compact" ? "active" : ""} onClick={() => toggleMediaLayout("compact")}>
                  {t("notch.mediaLayout.compact")}
                </span>
              </div>
            </SettingRow>

            <SettingRow icon={Palette} label={t("notch.artworkTheme")} desc={t("notch.artworkTheme.desc")}>
              <label className="toggle-switch">
                <input type="checkbox" checked={artworkThemeEnabled} onChange={toggleArtworkTheme} />
                <span className="slider"></span>
              </label>
            </SettingRow>

            <SettingRow icon={Sparkles} label={t("notch.ambientGlow")} desc={t("notch.ambientGlow.desc")}>
              <label className="toggle-switch">
                <input type="checkbox" checked={mediaAmbienceEnabled} onChange={toggleAmbience} />
                <span className="slider"></span>
              </label>
            </SettingRow>

            <SettingRow icon={Sparkle} label={t("notch.compactGlow")} desc={t("notch.compactGlow.desc")}>
              <label className="toggle-switch">
                <input type="checkbox" checked={mediaCompactGlowEnabled} onChange={toggleCompactGlow} />
                <span className="slider"></span>
              </label>
            </SettingRow>
          </>
        )}
      </SettingSection>

      <SettingSection id="notch-panels" title={t("notch.section.panels")}>
        <SettingRow icon={SlidersHorizontal} label={t("notch.panelControlCenter")} desc={t("notch.panelControlCenter.desc")}>
              <label className="toggle-switch">
                <input type="checkbox" checked={panelControlCenter} onChange={togglePanelControlCenter} />
                <span className="slider"></span>
              </label>
            </SettingRow>

        <SettingRow icon={Inbox} label={t("notch.panelShelf")} desc={t("notch.panelShelf.desc")}>
              <label className="toggle-switch">
                <input type="checkbox" checked={panelShelf} onChange={togglePanelShelf} />
                <span className="slider"></span>
              </label>
            </SettingRow>

        <SettingRow icon={ClipboardList} label={t("notch.panelClipboard")} desc={t("notch.panelClipboard.desc")}>
              <label className="toggle-switch">
                <input type="checkbox" checked={panelClipboard} onChange={togglePanelClipboard} />
                <span className="slider"></span>
              </label>
            </SettingRow>

        {panelClipboard && (
          <>
            <SettingRow icon={ListOrdered} label={t("notch.clipboardSize")} desc={t("notch.clipboardSize.desc")}>
              <Dropdown
                value={clipboardSize}
                onChange={changeClipboardSize}
                options={["10", "20", "30", "50", "100"].map((n) => ({ value: n, label: n }))}
              />
            </SettingRow>

            <SettingRow icon={Pin} label={t("notch.clipboardPersist")} desc={t("notch.clipboardPersist.desc")}>
              <label className="toggle-switch">
                <input type="checkbox" checked={clipboardPersist} onChange={toggleClipboardPersist} />
                <span className="slider"></span>
              </label>
            </SettingRow>
          </>
        )}

        {!legacyMode && (
          <>
            <SettingRow icon={MousePointer2} label={t("notch.hoverOpen")} desc={t("notch.hoverOpen.desc")}>
              <label className="toggle-switch">
                <input type="checkbox" checked={hoverOpen} onChange={toggleHoverOpen} />
                <span className="slider"></span>
              </label>
            </SettingRow>

            {hoverOpen && (
              <SettingRow icon={PanelTop} label={t("notch.hoverPanel")} desc={t("notch.hoverPanel.desc")}>
                <Dropdown
                  value={hoverPanel}
                  onChange={changeHoverPanel}
                  options={DEFAULT_PANEL_ORDER.map((id) => ({ value: id, label: t(`panel.${id}`) }))}
                />
              </SettingRow>
            )}
          </>
        )}

        <SettingRow icon={History} label={t("notch.rememberMode")} desc={t("notch.rememberMode.desc")}>
              <label className="toggle-switch">
                <input type="checkbox" checked={rememberMode} onChange={toggleRememberMode} />
                <span className="slider"></span>
              </label>
            </SettingRow>

        <SettingRow icon={ListOrdered} label={t("notch.panelOrder")} desc={t("notch.panelOrder.desc")} divider={false} stacked>
          <OrderList
            ids={panelOrder ?? DEFAULT_PANEL_ORDER}
            label={(id) => t(`panel.${id}`)}
            icon={(id) => {
              const Icon = panelIcon(id);
              return <Icon size={14} strokeWidth={2} />;
            }}
            onReorder={(order) => reorderPanels(order as PanelId[])}
            onReset={panelOrder ? resetPanelOrder : undefined}
            resetLabel={t("notch.panelOrder.reset")}
          />
        </SettingRow>
      </SettingSection>

      <SettingSection id="notch-cc" title={t("notch.section.cc")}>
        <ControlCenterConfig
          value={ccLayout}
          onChange={changeCcLayout}
          onReset={resetCcLayout}
          customized={Boolean(ccLayout.tiles || ccLayout.buttons || ccLayout.sliders)}
        />
      </SettingSection>

      <SettingSection id="notch-agents" title={t("notch.section.agents")}>
        <SettingRow icon={Bot} label={t("notch.agentApi")} desc={t("notch.agentApi.desc")}>
          <label className="toggle-switch">
            <input type="checkbox" checked={agentApi} onChange={toggleAgentApi} />
            <span className="slider"></span>
          </label>
        </SettingRow>
        <SettingRow icon={LayoutList} label={t("notch.agentStyle")} desc={t("notch.agentStyle.desc")} divider={false}>
          <div className="unit-toggle-minimal wide">
            <span className={agentStyle === "mascot" ? "active" : ""} onClick={() => changeAgentStyle("mascot")}>
              {t("notch.agentStyle.mascot")}
            </span>
            <span className={agentStyle === "bar" ? "active" : ""} onClick={() => changeAgentStyle("bar")}>
              {t("notch.agentStyle.bar")}
            </span>
          </div>
        </SettingRow>
        {agentApi && (
          <SettingRow icon={Gauge} label={t("notch.claudeUsage")} desc={t("notch.claudeUsage.desc")}>
            <label className="toggle-switch">
              <input type="checkbox" checked={claudeUsage} onChange={toggleClaudeUsage} />
              <span className="slider"></span>
            </label>
          </SettingRow>
        )}
        <AgentSounds />
      </SettingSection>

      <SettingSection id="notch-mic-cam" title={t("notch.section.micCam")}>
        <MicCamSettings />
      </SettingSection>

      <SettingSection id="notch-notifications" title={t("notch.section.notifications")}>
        <NotchNotifications legacyMode={legacyMode} />
      </SettingSection>

      <SettingSection id="notch-controls" title={t("notch.section.controls")}>
        <SettingRow icon={Volume2} label={t("notch.scrollVolume")} desc={t("notch.scrollVolume.desc")}>
              <label className="toggle-switch">
                <input type="checkbox" checked={notchScrollVolume} onChange={toggleNotchScrollVolume} />
                <span className="slider"></span>
              </label>
            </SettingRow>

        <NotchDownloads legacyMode={legacyMode} />

        <SettingRow icon={Pin} label={t("notch.shelfPersist")} desc={t("notch.shelfPersist.desc")} divider={false}>
              <label className="toggle-switch">
                <input type="checkbox" checked={shelfPersist} onChange={toggleShelfPersist} />
                <span className="slider"></span>
              </label>
            </SettingRow>
      </SettingSection>

      <SettingSection id="notch-weather" title={t("notch.weather.group")}>
        <SettingRow icon={CloudSun} label={t("notch.weather")} desc={cityName || t("notch.weather.auto")}>
          <div className="weather-controls">
            <div className="unit-toggle-minimal" onClick={toggleTempUnit}>
              <span className={!tempUnitFahrenheit ? "active" : ""}>C</span>
              <span className={tempUnitFahrenheit ? "active" : ""}>F</span>
            </div>
            <label className="toggle-switch">
              <input type="checkbox" checked={weatherEnabled} onChange={toggleWeather} />
              <span className="slider"></span>
            </label>
          </div>
        </SettingRow>

        {weatherEnabled && (
          <div className="manual-city-input">
            <div className="city-input-row">
              <input
                type="text"
                placeholder={t("notch.weather.search")}
                value={cityQuery}
                onChange={(e) => setCityQuery(e.target.value)}
                onFocus={() => citySearchResults.length > 0 && setShowCityDropdown(true)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && citySearchResults.length > 0) {
                    e.preventDefault();
                    selectCity(citySearchResults[0]);
                  }
                  if (e.key === "Escape") {
                    setShowCityDropdown(false);
                  }
                }}
                onBlur={() => setTimeout(() => {
                  setShowCityDropdown(false);
                  if (cityQuery !== cityName && citySearchResults.length > 0) selectCity(citySearchResults[0]);
                }, 150)}
              />
              {(cityName || cityQuery) && (
                <button
                  className="city-clear-btn"
                  onMouseDown={(e) => {
                    e.preventDefault();
                    handleCityClear();
                  }}
                  title={t("notch.weather.clear")}
                >
                  <X size={10} strokeWidth={2.5} />
                </button>
              )}
            </div>
            {showCityDropdown && citySearchResults.length > 0 && (
              <div className="city-dropdown">
                {citySearchResults.map((city) => (
                  <button
                    key={`${city.name}-${city.latitude}`}
                    className="city-dropdown-item"
                    onMouseDown={(e) => {
                      e.preventDefault();
                      selectCity(city);
                    }}
                  >
                    <span className="city-dropdown-name">{city.name}</span>
                    <span className="city-dropdown-country">{city.country}</span>
                  </button>
                ))}
              </div>
            )}
          </div>
        )}
      </SettingSection>

      <SettingSection id="notch-hotkey" title={t("notch.hotkey.group")}>
        <NotchHotkey />
      </SettingSection>

      <SettingSection id="notch-widgets" title={t("notch.widgets.group")}>
        <StatusWidgetConfig value={statusWidgets} onChange={handleWidgetsChange} hasBattery={hasBattery} />
        <NetWidgetMode />
      </SettingSection>
    </>
  );
}
