import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "sonner";
import { commands } from "@/bindings";
import { Button } from "../ui/Button";
import { Input } from "../ui/Input";
import { SettingsGroup } from "../ui/SettingsGroup";

// Mirrors src-tauri/src/remote_asr.rs (kept local so bindings.ts stays untouched).
interface RemoteAsrConfig {
  enabled: boolean;
  url: string;
  token: string;
  mode: "clean" | "raw";
}

interface RemoteAsrStatus {
  reachable: boolean;
  whisper_loaded: boolean;
  llm_loaded: boolean;
  llm: string;
  error: string | null;
}

const EMPTY: RemoteAsrConfig = {
  enabled: false,
  url: "",
  token: "",
  mode: "clean",
};

interface RemoteAsrProps {
  /** Called after the remote server was switched on (used by onboarding). */
  onEnabled?: () => void;
}

export const RemoteAsr: React.FC<RemoteAsrProps> = ({ onEnabled }) => {
  const { t } = useTranslation();
  const [cfg, setCfg] = useState<RemoteAsrConfig>(EMPTY);
  const [loaded, setLoaded] = useState(false);
  const [status, setStatus] = useState<RemoteAsrStatus | null>(null);
  const [busy, setBusy] = useState<"warmup" | "unload" | null>(null);

  useEffect(() => {
    commands.getAppSettings().then((res) => {
      if (res.status === "ok") {
        const remote = (res.data as unknown as { remote_asr?: RemoteAsrConfig })
          .remote_asr;
        if (remote) setCfg({ ...EMPTY, ...remote });
      }
      setLoaded(true);
    });
  }, []);

  const save = useCallback(async (next: RemoteAsrConfig) => {
    setCfg(next);
    await invoke("set_remote_asr_settings", { remoteAsr: next });
  }, []);

  const refresh = useCallback(async () => {
    setStatus(await invoke<RemoteAsrStatus>("remote_asr_status"));
  }, []);

  useEffect(() => {
    if (!loaded || !cfg.url.trim()) {
      setStatus(null);
      return;
    }
    refresh();
    const id = setInterval(refresh, 5000);
    return () => clearInterval(id);
  }, [loaded, cfg.url, cfg.token, refresh]);

  const control = async (kind: "warmup" | "unload") => {
    setBusy(kind);
    try {
      setStatus(
        await invoke<RemoteAsrStatus>(
          kind === "warmup" ? "remote_asr_warmup" : "remote_asr_unload",
        ),
      );
    } catch (e) {
      toast.error(t("settings.remoteAsr.requestFailed", { error: String(e) }));
    } finally {
      setBusy(null);
    }
  };

  const toggle = async () => {
    const next = { ...cfg, enabled: !cfg.enabled };
    await save(next);
    if (next.enabled) onEnabled?.();
  };

  const statusText = !status
    ? t("settings.remoteAsr.status.unknown")
    : !status.reachable
      ? t("settings.remoteAsr.status.unreachable")
      : t("settings.remoteAsr.status.ok", {
          whisper: status.whisper_loaded ? "✓" : "—",
          llm: status.llm_loaded ? "✓" : "—",
          model: status.llm,
        });

  return (
    <SettingsGroup
      title={t("settings.remoteAsr.title")}
      description={t("settings.remoteAsr.description")}
    >
      <div className="p-4 space-y-3">
        <Input
          className="w-full"
          placeholder="http://192.168.1.20:8765"
          value={cfg.url}
          onChange={(e) => setCfg({ ...cfg, url: e.target.value })}
          onBlur={() => save(cfg)}
        />
        <Input
          className="w-full"
          type="password"
          placeholder={t("settings.remoteAsr.token")}
          value={cfg.token}
          onChange={(e) => setCfg({ ...cfg, token: e.target.value })}
          onBlur={() => save(cfg)}
        />
        <div className="flex items-center gap-2">
          <label className="text-sm" htmlFor="remote-asr-mode">
            {t("settings.remoteAsr.mode.label")}
          </label>
          <select
            id="remote-asr-mode"
            className="px-2 py-1 text-sm bg-mid-gray/10 border border-mid-gray/80 rounded-md"
            value={cfg.mode}
            onChange={(e) =>
              save({ ...cfg, mode: e.target.value as RemoteAsrConfig["mode"] })
            }
          >
            <option value="clean">{t("settings.remoteAsr.mode.clean")}</option>
            <option value="raw">{t("settings.remoteAsr.mode.raw")}</option>
          </select>
        </div>
        <div className="text-xs text-mid-gray">{statusText}</div>
        <div className="flex flex-wrap gap-2">
          <Button
            variant={cfg.enabled ? "primary" : "primary-soft"}
            size="sm"
            disabled={!cfg.url.trim()}
            onClick={toggle}
          >
            {cfg.enabled
              ? t("settings.remoteAsr.disable")
              : t("settings.remoteAsr.enable")}
          </Button>
          <Button
            variant="secondary"
            size="sm"
            disabled={busy !== null || !status?.reachable}
            onClick={() => control("warmup")}
          >
            {busy === "warmup"
              ? t("settings.remoteAsr.warming")
              : t("settings.remoteAsr.warmup")}
          </Button>
          <Button
            variant="secondary"
            size="sm"
            disabled={busy !== null || !status?.reachable}
            onClick={() => control("unload")}
          >
            {t("settings.remoteAsr.unload")}
          </Button>
        </div>
      </div>
    </SettingsGroup>
  );
};
