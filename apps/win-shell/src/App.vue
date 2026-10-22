<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";

type ThemePref = "light" | "dark" | "system";

type NearbyPeer = {
  device_id: string;
  alias: string;
  host: string;
  port: number;
  trusted: boolean;
  last_seen_ms: number;
};

type SessionStatus = {
  state: string;
  paired: boolean;
  listen_port: number;
  listening: boolean;
  pairing_code: string | null;
  peer_device_id: string | null;
  peer_alias: string | null;
  peer_host: string | null;
  peer_port: number | null;
  device_id: string;
  alias: string;
  local_ipv4: string | null;
  connection: string;
  /** "unpaired" | "timeout" | "closed" — null while connected. */
  disconnect_reason?: string | null;
  nearby: NearbyPeer[];
  banner: string;
  transfer_done: number;
  transfer_total: number;
  transfer_bps: number;
  transfer_name: string;
  last_send_probe_bps?: number;
  last_send_transfer_bps?: number;
  last_send_bytes?: number;
};

type JobCard = {
  id: string;
  kind: "send" | "recv" | "clip";
  title: string;
  detail: string;
  status: "queued" | "active" | "done" | "fail";
  path?: string;
};

const THEME_KEY = "lanbridge.theme";
const DEST_KEY = "lanbridge.destRoot";

/** System toast; never put clipboard body or file bytes in `body`. Vendor: plugin-notification.md */
async function notifyUser(title: string, body: string): Promise<void> {
  try {
    let ok = await isPermissionGranted();
    if (!ok) {
      ok = (await requestPermission()) === "granted";
    }
    if (!ok) return;
    sendNotification({ title, body });
  } catch {
    /* ignore — toast is best-effort */
  }
}

const themePref = ref<ThemePref>("system");
const busy = ref(false);
const error = ref("");
const st = ref<SessionStatus | null>(null);
const destRoot = ref("");
const destLabel = ref("");
const pairCodeInput = ref("");
const pairTargetId = ref("");
const peerHostEdit = ref("");
const peerPortEdit = ref(37532);
const showManualPair = ref(false);
/** User collapsed manual IP — do not auto-reopen on every status poll. */
const manualPairUserClosed = ref(false);
const showAdvanced = ref(false);
const listenPortEdit = ref(0);
const clipAuto = ref(true);
const clipHint = ref(
  "配对后：本机复制自动同步到手机（手机需保活/前台）；对端复制切回手机 App 后同步到本机",
);
const sendQueue = ref<{ path: string; name: string }[]>([]);
const jobs = ref<JobCard[]>([]);
const activeJobId = ref<string | null>(null);

let serveLoop = false;
let serveAbort = false;
let pollTimer: number | null = null;
let autoProbeTimer: number | null = null;
let autoProbeBusy = false;
let lastAutoProbeMs = 0;
let pairWaitRunning = false;
let lastClipText = "";

function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n < 0) return "—";
  if (n < 1024) return `${Math.round(n)} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(2) : v < 100 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

function formatRate(bps: number): string {
  if (!bps || bps <= 0) return "";
  return `${formatBytes(bps)}/s`;
}

function progressDetail(stSnap: SessionStatus | null, fallback: string): string {
  if (!stSnap) return fallback;
  const total = stSnap.transfer_total || 0;
  const done = stSnap.transfer_done || 0;
  if (!total && !done) return fallback;
  if (!total) {
    return `${formatBytes(done)} 已收发`;
  }
  const pct = Math.min(100, Math.round((done / total) * 100));
  const rate = formatRate(stSnap.transfer_bps);
  const phase = done === 0 ? "准备中" : `${pct}%`;
  return `${phase} · ${formatBytes(done)} / ${formatBytes(total)}${rate ? ` · ${rate}` : ""}`;
}

const liveBanner = computed(() => st.value?.banner || "…");
const connectionLabel = computed(() => {
  switch (st.value?.connection) {
    case "connected":
      return "已连接";
    case "pairing":
      // Consistent with the phone and with the banner: we are waiting for the peer to type our code.
      return "等待配对";
    case "searching":
      return "已上线 · 等待配对";
    default:
      // Peer said Bye: trust is gone, this is a deliberate unpair, not a network blip.
      if (st.value?.disconnect_reason === "unpaired") return "对端已取消配对";
      // Paired + listening but no recent heartbeat: still operable via TCP.
      if (st.value?.paired && st.value?.listening) return "已配对 · 未探测到对端";
      return "未配对";
  }
});
/** Short form for the sidebar footer / chips (the long label wraps). */
const shortStateLabel = computed(() => {
  if (connectionLabel.value === "已连接") return "已连接";
  if (st.value?.disconnect_reason === "unpaired") return "对端已取消配对";
  if (st.value?.connection === "pairing") return "等待配对";
  if (st.value?.paired && st.value?.listening) return "对端离线";
  if (st.value?.listening) return "未配对";
  return "未配对";
});
const myCode = computed(() => st.value?.pairing_code || "");
const localIp = computed(() => st.value?.local_ipv4 || "");
const nearby = computed(() => st.value?.nearby ?? []);
/** Ops need trust + local listen; do NOT require UDP "connected" (often filtered). */
const canSend = computed(
  () => !!st.value?.paired && !!st.value?.listening,
);
/** Trust + listen is enough; UDP "connected" is often filtered on hotspot. */
const linkOk = computed(
  () =>
    connectionLabel.value === "已连接" ||
    connectionLabel.value.startsWith("已配对"),
);

function basenameOf(path: string): string {
  const norm = path.replace(/\\/g, "/");
  const i = norm.lastIndexOf("/");
  return i >= 0 ? norm.slice(i + 1) : norm;
}

function pushJob(job: JobCard) {
  jobs.value = [job, ...jobs.value].slice(0, 12);
}

function updateJob(id: string, patch: Partial<JobCard>) {
  jobs.value = jobs.value.map((j) => (j.id === id ? { ...j, ...patch } : j));
}

async function applyTheme(pref: ThemePref) {
  themePref.value = pref;
  localStorage.setItem(THEME_KEY, pref);
  const root = document.documentElement;
  if (pref === "system") {
    root.removeAttribute("data-theme");
    try {
      await getCurrentWindow().setTheme(null);
    } catch {
      /* preview */
    }
  } else {
    root.setAttribute("data-theme", pref);
    try {
      await getCurrentWindow().setTheme(pref);
    } catch {
      /* preview */
    }
  }
}

async function refreshStatus() {
  try {
    const snap = await invoke<SessionStatus>("session_status");
    const prevPaired = st.value?.paired;
    st.value = snap;
    if (snap.banner?.includes("网络已切换")) {
      manualPairUserClosed.value = false;
      showManualPair.value = true;
      clipHint.value =
        "换网后旧对端 IP 失效：可先「检测对端」；失败则清除配对，用手填新 IP";
      // Do not keep auto-filling the obsolete router IP into the manual field.
      if (snap.peer_host && peerHostEdit.value === snap.peer_host) {
        peerHostEdit.value = "";
      }
    } else if (snap.peer_host && !peerHostEdit.value.trim()) {
      peerHostEdit.value = snap.peer_host;
    }
    if (snap.peer_port && snap.peer_port > 0) {
      peerPortEdit.value = snap.peer_port;
    }
    if (
      !snap.paired &&
      (snap.nearby?.length ?? 0) === 0 &&
      !manualPairUserClosed.value
    ) {
      showManualPair.value = true;
    }
    const live =
      snap.state === "transferring_in" || snap.state === "transferring_out";
    if (live) {
      // Show a job even before total is known (establish / FileOffer phase).
      if (snap.state === "transferring_in") {
        const title =
          snap.transfer_name && snap.transfer_name.trim().length > 0
            ? snap.transfer_name.trim()
            : "接收中…";
        if (
          !activeJobId.value ||
          !jobs.value.some((j) => j.id === activeJobId.value && j.kind === "recv" && j.status === "active")
        ) {
          const id = `recv-live-${Date.now()}`;
          activeJobId.value = id;
          pushJob({
            id,
            kind: "recv",
            title,
            detail: progressDetail(snap, "接收中…"),
            status: "active",
          });
        } else {
          updateJob(activeJobId.value, {
            status: "active",
            title,
            detail: progressDetail(snap, "接收中…"),
          });
        }
      } else if (snap.state === "transferring_out" && activeJobId.value) {
        const cur = jobs.value.find((j) => j.id === activeJobId.value);
        if (cur && cur.status === "active") {
          const title =
            snap.transfer_name && snap.transfer_name.trim().length > 0
              ? snap.transfer_name.trim()
              : cur.title;
          updateJob(activeJobId.value, {
            status: "active",
            title,
            detail: progressDetail(snap, "传输中…"),
          });
        }
      }
    }
    if (!busy.value) {
      if (snap.paired && !prevPaired) {
        void notifyUser("LanBridge", "已配对");
        void startServeLoop();
      }
      if (snap.listening && !snap.paired && snap.connection !== "pairing") {
        void startPairWaitIfNeeded();
      }
    }
  } catch {
    /* ignore transient poll errors */
  }
}

async function ensureOnline() {
  if (st.value?.listening) return;
  st.value = await invoke<SessionStatus>("session_go_online", {
    listenPort: listenPortEdit.value > 0 ? listenPortEdit.value : 0,
  });
}

/** User action: TLS probe so「已配对未探测/离线」can become「已连接」. */
async function probePeer() {
  if (!st.value?.paired) {
    error.value = "尚未配对";
    return;
  }
  busy.value = true;
  error.value = "";
  try {
    await ensureOnline();
    let last = "";
    for (let attempt = 0; attempt < 8; attempt++) {
      try {
        st.value = await invoke<SessionStatus>("session_probe_peer");
        error.value = "";
        clipHint.value =
          st.value.connection === "connected" ? "对端可达" : "已探测，请再试收发";
        return;
      } catch (e) {
        last = String(e);
        if ((last.includes("busy") || last.includes("rc=45")) && attempt < 7) {
          await new Promise((r) => setTimeout(r, 350));
          continue;
        }
        break;
      }
    }
    error.value = "";
    clipHint.value = last.includes("peer_unreachable")
      ? "对端未响应，确认对方已打开 LanBridge"
      : last.slice(0, 160) || "检测失败";
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}

/** Wipe trust so the code box / pair flow comes back (fixes one-sided reinstall). */
async function clearPair() {
  busy.value = true;
  error.value = "";
  serveAbort = true;
  try {
    // Bye first: without it the phone keeps「已连接」until the heartbeat window expires.
    st.value = await invoke<SessionStatus>("session_unpair");
    await ensureOnline();
    void startPairWaitIfNeeded();
    clipHint.value =
      "已清除配对并通知对端。填对方 6 位码后点「确认配对」；附近为空时可手填对方 IP";
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}

async function ensureRecvDir() {
  const saved = localStorage.getItem(DEST_KEY);
  if (saved) {
    destRoot.value = saved;
    destLabel.value = saved;
    return;
  }
  const d = await invoke<string>("default_recv_dir");
  destRoot.value = d;
  destLabel.value = d;
  localStorage.setItem(DEST_KEY, d);
}

async function bootstrap() {
  error.value = "";
  try {
    st.value = await invoke<SessionStatus>("session_open", { alias: "PC" });
    await ensureOnline();
    await ensureRecvDir();
    await refreshStatus();
    void startPairWaitIfNeeded();
    void startServeLoop();
    startPoll();
  } catch (e) {
    error.value = String(e);
  }
}

function startPoll() {
  stopPoll();
  pollTimer = window.setInterval(() => {
    void refreshStatus().catch(() => {});
    void maybeAutoProbe();
  }, 400);
}

function stopPoll() {
  if (pollTimer != null) {
    clearInterval(pollTimer);
    pollTimer = null;
  }
  if (autoProbeTimer != null) {
    clearTimeout(autoProbeTimer);
    autoProbeTimer = null;
  }
}

/** Hotspot often blocks UDP beacons to the PC; TCP probe keeps「已连接」honest. */
async function maybeAutoProbe() {
  const s = st.value;
  if (!s?.paired || !s.listening || busy.value || autoProbeBusy) return;
  if (s.connection === "connected" || s.connection === "pairing") return;
  const now = Date.now();
  if (now - lastAutoProbeMs < 8000) return;
  lastAutoProbeMs = now;
  autoProbeBusy = true;
  try {
    st.value = await invoke<SessionStatus>("session_probe_peer");
  } catch {
    /* keep prior status; user can tap 检测对端 */
  } finally {
    autoProbeBusy = false;
  }
}

async function startPairWaitIfNeeded() {
  if (pairWaitRunning) return;
  if (!st.value?.listening || st.value.paired || st.value.connection === "pairing") return;
  pairWaitRunning = true;
  try {
    st.value = await invoke<SessionStatus>("session_begin_pair");
  } catch (e) {
    error.value = String(e);
  } finally {
    pairWaitRunning = false;
  }
}

async function pairWithNearby(peer: NearbyPeer) {
  const code = pairCodeInput.value.trim() || window.prompt(`输入「${peer.alias}」屏幕上的 6 位配对码`) || "";
  if (!code.trim()) {
    error.value = "请先填写对方屏幕上的 6 位配对码";
    return;
  }
  if (code.trim().length < 6) {
    error.value = "配对码须为 6 位";
    return;
  }
  busy.value = true;
  error.value = "";
  pairTargetId.value = peer.device_id;
  try {
    st.value = await invoke<SessionStatus>("session_pair_nearby", {
      deviceId: peer.device_id,
      code: code.trim(),
    });
    pairCodeInput.value = "";
    void notifyUser("LanBridge", `已与 ${peer.alias || peer.device_id} 配对`);
    void startServeLoop();
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
    pairTargetId.value = "";
  }
}

/** Confirm code + pair nearby, or fall back to manual IP when nearby is empty. */
async function confirmPairWithNearby() {
  const code = pairCodeInput.value.trim();
  if (code.length < 6) {
    error.value = "请先填满对方 6 位配对码";
    return;
  }
  const target =
    nearby.value.find((p) => !p.trusted) ||
    (nearby.value.length === 1 ? nearby.value[0] : undefined);
  if (!target) {
    manualPairUserClosed.value = false;
    showManualPair.value = true;
    if (peerHostEdit.value.trim()) {
      await pairWithManual();
      return;
    }
    error.value =
      "附近暂无设备。请在下方填写对方本机 IP（手机屏上「本机 IP」），再点「用 IP 配对」";
    return;
  }
  if (target.trusted) {
    await probePeer();
    return;
  }
  await pairWithNearby(target);
}

async function pairWithManual() {
  const code = pairCodeInput.value.trim();
  const host = peerHostEdit.value.trim();
  if (code.length < 6) {
    error.value = "请先填满对方 6 位配对码";
    return;
  }
  if (!host) {
    error.value = "请填写对方本机 IP";
    manualPairUserClosed.value = false;
    showManualPair.value = true;
    return;
  }
  const port = peerPortEdit.value > 0 ? peerPortEdit.value : 37532;
  busy.value = true;
  error.value = "";
  try {
    st.value = await invoke<SessionStatus>("session_pair", {
      host,
      port,
      code,
    });
    pairCodeInput.value = "";
    void notifyUser("LanBridge", `已与 ${host} 配对`);
    void startServeLoop();
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}

async function refreshNearby() {
  error.value = "";
  await refreshStatus();
  if (!nearby.value.length) {
    manualPairUserClosed.value = false;
    showManualPair.value = true;
    clipHint.value =
      "附近仍无设备（热点常拦广播）。看对方屏上的本机 IP，填到下方手填区";
  }
}

function toggleManualPair() {
  showManualPair.value = !showManualPair.value;
  manualPairUserClosed.value = !showManualPair.value;
}

async function pickRecvDir() {
  const p = await invoke<string | null>("pick_save_folder");
  if (!p) return;
  destRoot.value = p;
  destLabel.value = p;
  localStorage.setItem(DEST_KEY, p);
  void startServeLoop();
}

async function openRecvFolder() {
  if (!destRoot.value) return;
  try {
    await invoke("open_path", { path: destRoot.value });
  } catch (e) {
    error.value = String(e);
  }
}

async function openJobPath(j: JobCard) {
  const target = j.path || (j.kind === "recv" ? destRoot.value : "");
  if (!target) return;
  try {
    await invoke("open_path", { path: target });
  } catch (e) {
    error.value = String(e);
  }
}

async function pickSendFiles() {
  const paths = await invoke<string[]>("pick_open_files");
  if (!paths?.length) return;
  for (const path of paths) {
    sendQueue.value.push({ path, name: basenameOf(path) });
  }
}

function clearSendQueue() {
  sendQueue.value = [];
}

async function sendQueued() {
  if (!canSend.value || !sendQueue.value.length) return;
  busy.value = true;
  error.value = "";
  const batch = [...sendQueue.value];
  sendQueue.value = [];
  const jobIds: string[] = [];
  for (let i = 0; i < batch.length; i++) {
    const f = batch[i];
    const id = `send-${Date.now()}-${i}-${f.name}`;
    jobIds.push(id);
    pushJob({
      id,
      kind: "send",
      title: f.name,
      detail: `排队 ${i + 1}/${batch.length}`,
      status: "active",
      path: f.path,
    });
  }
  let ok = 0;
  for (let i = 0; i < batch.length; i++) {
    const f = batch[i];
    const id = jobIds[i];
    activeJobId.value = id;
    updateJob(id, {
      status: "active",
      detail: `发送中… (${i + 1}/${batch.length})`,
    });
    try {
      let n = 0;
      for (let attempt = 0; attempt < 16; attempt++) {
        try {
          n = await invoke<number>("session_send_file", {
            filePath: f.path,
            remoteName: f.name,
          });
          break;
        } catch (e) {
          const s = String(e);
          const soft =
            s.includes("busy") ||
            s.includes("rc=45") ||
            s.includes("peer_unreachable");
          if (soft && attempt < 15) {
            await new Promise((r) => setTimeout(r, 400 + attempt * 100));
            continue;
          }
          throw e;
        }
      }
      ok++;
      updateJob(id, { status: "done", detail: `已发送 · ${formatBytes(n)}` });
    } catch (e) {
      const raw = String(e);
      const msg = raw.includes("peer_unreachable")
        ? linkOk.value
          ? "对端未接受连接（可能正忙于发送），请稍后再试"
          : "连不上对端，确认对方已上线"
        : raw;
      updateJob(id, { status: "fail", detail: msg });
      // Don't sticky-red contradict the live banner — job card carries the failure.
      error.value = linkOk.value ? "" : msg;
      sendQueue.value = batch.slice(ok);
      break;
    }
  }
  activeJobId.value = null;
  busy.value = false;
  if (ok > 0) {
    void notifyUser("LanBridge", `已发送 ${ok}/${batch.length} 个文件`);
  }
  await refreshStatus();
}

async function startServeLoop() {
  if (serveLoop) return;
  if (!destRoot.value) return;
  serveLoop = true;
  serveAbort = false;
  while (!serveAbort) {
    // Never skip accept while UI "busy": outbound send holds the session lock;
    // skipping serve here means the peer cannot connect (UDP still says online).
    const cur = st.value;
    if (!cur?.listening || !cur.paired) break;
    try {
      const ev = await invoke<{
        kind: string;
        bytes: number;
        path?: string;
        preview?: string;
        message?: string;
        text?: string;
        png_base64?: string;
      }>("session_serve_once", { destRoot: destRoot.value });
      const id = `recv-${Date.now()}`;
      if (ev.kind === "file") {
        if (activeJobId.value?.startsWith("recv-live-")) {
          updateJob(activeJobId.value, {
            status: "done",
            title: basenameOf(ev.path || "文件"),
            detail: `已接收 · ${formatBytes(ev.bytes)}`,
            path: ev.path,
          });
          activeJobId.value = null;
        } else {
          pushJob({
            id,
            kind: "recv",
            title: basenameOf(ev.path || "文件"),
            detail: `已接收 · ${formatBytes(ev.bytes)}`,
            status: "done",
            path: ev.path,
          });
        }
        void notifyUser("LanBridge", `已接收 ${basenameOf(ev.path || "文件")}`);
      } else if (ev.kind === "clipboard_text") {
        // Prefer Rust main-thread write (message); frontend sync write as fallback.
        const full = String(
          (ev as { text?: string }).text ??
            (ev as { Text?: string }).Text ??
            "",
        );
        let msg =
          ev.message ||
          (full.trim().length > 0 ? "已写入系统剪贴板" : "接收成功但正文为空");
        if (full.trim().length > 0) {
          lastClipText = full.trim();
          clipInboundQuietUntil = Date.now() + 4000;
          void invoke<number>("read_os_clipboard_seq")
            .then((seq) => {
              if (seq !== 0) lastClipSeq = seq;
            })
            .catch(() => {});
          if (!ev.message || String(ev.message).includes("失败")) {
            try {
              await invoke("copy_text_to_os_clipboard", { text: full.trim() });
              msg = "已写入系统剪贴板";
            } catch (e) {
              msg = `接收成功但写系统剪贴板失败: ${String(e).slice(0, 80)}`;
            }
          }
        }
        pushJob({
          id,
          kind: "clip",
          title: "收到文本",
          detail: msg,
          status: msg.includes("失败") ? "fail" : "done",
        });
        clipHint.value = msg;
      } else if (ev.kind === "clipboard_png") {
        // The backend writes the image to the OS clipboard itself (main thread, decoded bytes
        // only). It used to also ship a ~20 MB base64 string here and the UI echoed it back,
        // which froze the window for seconds on a 4K screenshot — keep the payload out of IPC.
        const msg = ev.message || "已写入系统剪贴板";
        clipInboundQuietUntil = Date.now() + 4000;
        // Remember the sequence number we just produced, otherwise the poll would echo this
        // image straight back to the phone once the quiet window expires.
        void invoke<number>("read_os_clipboard_seq")
          .then((seq) => {
            if (seq !== 0) lastClipSeq = seq;
          })
          .catch(() => {});
        pushJob({
          id,
          kind: "clip",
          title: "收到图片",
          detail: msg,
          status: msg.includes("失败") ? "fail" : "done",
        });
        clipHint.value = msg;
      } else if (ev.kind === "heartbeat" || ev.kind === "peer_closed") {
        // Core-side liveness / probe teardown: no job card, no banner churn.
        continue;
      } else if (ev.kind === "peer_left") {
        // Peer unpaired: core already wiped trust, so the pair flow is the next step.
        void notifyUser("LanBridge", "对端已取消配对");
        clipHint.value = "对端已取消配对：请重新填 6 位码配对";
        error.value = "";
        continue;
      } else {
        pushJob({
          id,
          kind: "recv",
          title: "入站异常",
          detail: ev.message ?? "错误",
          status: "fail",
        });
      }
      await refreshStatus().catch(() => {});
      void refreshHistory();
    } catch (e) {
      const s = String(e);
      if (serveAbort || s.includes("not_online") || s.includes("会话")) break;
      if (
        s.includes("timeout") ||
        s.includes("timed out") ||
        s.includes("busy") ||
        s.includes("waiting for trusted peer")
      ) {
        continue;
      }
      if (
        s.includes("close_notify") ||
        s.includes("unexpected eof") ||
        s.includes("UnexpectedEof") ||
        s.includes("connection closed") ||
        s.includes("Connection reset") ||
        s.includes("peer closed") ||
        s.includes("强迫关闭") ||
        s.includes("10054") ||
        s.includes("forcibly closed") ||
        // TLS-layer failures at accept are peer/protocol conditions, not user-actionable:
        // a cancelled heartbeat dial (radio wake-up / busy accept), a garbage probe, an alert…
        s.includes("tls:")
      ) {
        // probe_peer / soft TLS close — keep listening silently.
        error.value = "";
        continue;
      }
      if (s.includes("incomplete") || s.includes("Incomplete")) {
        if (activeJobId.value?.startsWith("recv-live-")) {
          updateJob(activeJobId.value, {
            status: "fail",
            detail: "传输未完成，已丢弃残缺文件（请重发）",
          });
          activeJobId.value = null;
        } else {
          pushJob({
            id: `recv-err-${Date.now()}`,
            kind: "recv",
            title: "接收失败",
            detail: "传输未完成，已丢弃残缺文件（请重发）",
            status: "fail",
          });
        }
        error.value = "";
        continue;
      }
      // Soft: log as job, keep listening — do not freeze the session behind a red banner.
      pushJob({
        id: `recv-err-${Date.now()}`,
        kind: "recv",
        title: "入站异常",
        detail: s.slice(0, 160),
        status: "fail",
      });
      error.value = "";
      await new Promise((r) => setTimeout(r, 400));
      continue;
    }
  }
  serveLoop = false;
  // Auto-restart if still paired/online (e.g. transient break).
  if (!serveAbort && st.value?.paired && st.value?.listening && destRoot.value) {
    void startServeLoop();
  }
}

async function pushClipboardOnce() {
  if (!canSend.value) return;
  busy.value = true;
  error.value = "";
  try {
    // Let in-flight serve_once release the session lock.
    await new Promise((r) => setTimeout(r, 200));
    let view: { preview: string; byte_len: number } | null = null;
    for (let attempt = 0; attempt < 12; attempt++) {
      try {
        view = await invoke("session_send_clipboard_text", { text: "" });
        break;
      } catch (e) {
        const s = String(e);
        if ((s.includes("busy") || s.includes("rc=45")) && attempt < 11) {
          await new Promise((r) => setTimeout(r, 350));
          continue;
        }
        throw e;
      }
    }
    if (!view) throw new Error("剪贴板发送失败");
    try {
      lastClipText = await invoke<string>("read_os_clipboard_text");
    } catch {
      /* ignore */
    }
    clipHint.value = `已推送文本（${formatBytes(view.byte_len)}）`;
    pushJob({
      id: `clip-${Date.now()}`,
      kind: "clip",
      title: "推送文本",
      detail: view.preview.slice(0, 64) || formatBytes(view.byte_len),
      status: "done",
    });
  } catch (e) {
    error.value = String(e);
    pushJob({
      id: `clip-${Date.now()}`,
      kind: "clip",
      title: "推送失败",
      detail: String(e).slice(0, 120),
      status: "fail",
    });
  } finally {
    busy.value = false;
  }
}

/** Auto clip: sequence-number gate + fast poll; does not hold UI `busy`. */
let clipInboundQuietUntil = 0;
let clipSending = false;
let lastClipSeq = 0;
async function clipPollTick() {
  if (!clipAuto.value || !canSend.value || clipSending) return;
  if (Date.now() < clipInboundQuietUntil) return;
  try {
    const seq = await invoke<number>("read_os_clipboard_seq");
    if (seq !== 0 && seq === lastClipSeq) return;
    const text = await invoke<string>("read_os_clipboard_text");
    if (seq !== 0) lastClipSeq = seq;
    if (!text) {
      // Screenshot / "copy image": clipboard holds pixels, not text.
      // A fresh screenshot file was just pushed — do not send the same image twice.
      if (Date.now() - lastScreenshotSentAt < 4000) return;
      const hasImage = await invoke<boolean>("os_clipboard_has_image");
      if (!hasImage) return;
      clipSending = true;
      try {
        await new Promise((r) => setTimeout(r, 80));
        await pushClipboardImageOnce();
      } catch (e) {
        // Let the next tick retry: the clipboard has not changed but the send failed.
        lastClipSeq = 0;
        clipHint.value = `图片同步失败：${String(e).slice(0, 80)}`;
      } finally {
        clipSending = false;
      }
      return;
    }
    if (text === lastClipText) return;
    clipSending = true;
    await new Promise((r) => setTimeout(r, 80));
    let view: { preview: string; byte_len: number } | null = null;
    for (let attempt = 0; attempt < 12; attempt++) {
      try {
        view = await invoke("session_send_clipboard_text", { text });
        break;
      } catch (e) {
        const s = String(e);
        if ((s.includes("busy") || s.includes("rc=45")) && attempt < 11) {
          await new Promise((r) => setTimeout(r, 280));
          continue;
        }
        throw e;
      }
    }
    if (!view) return;
    lastClipText = text;
    void refreshHistory();
    clipHint.value = `已自动同步到手机（${formatBytes(view.byte_len)}）`;
    pushJob({
      id: `clip-${Date.now()}`,
      kind: "clip",
      title: "自动同步",
      detail: view.preview.slice(0, 48),
      status: "done",
    });
  } catch (e) {
    clipHint.value = `自动同步失败：${String(e).slice(0, 80)}`;
    void notifyUser("LanBridge", "自动剪贴板同步失败");
  } finally {
    clipSending = false;
  }
}

/** Empty filePath => "read the image currently in the Windows clipboard" (handled in Rust). */
async function pushClipboardImageOnce(): Promise<void> {
  let view: { preview: string; byte_len: number } | null = null;
  for (let attempt = 0; attempt < 12; attempt++) {
    try {
      view = await invoke("session_send_clipboard_image", { filePath: "" });
      break;
    } catch (e) {
      const s = String(e);
      if ((s.includes("busy") || s.includes("rc=45")) && attempt < 11) {
        await new Promise((r) => setTimeout(r, 280));
        continue;
      }
      throw e;
    }
  }
  if (!view) return;
  void refreshHistory();
  clipHint.value = `已自动同步图片（${formatBytes(view.byte_len)}）`;
  pushJob({
    id: `clip-img-${Date.now()}`,
    kind: "clip",
    title: "自动同步（图片）",
    detail: view.preview.slice(0, 48),
    status: "done",
  });
}

/** Last screenshot-file push: avoids echoing the same image through the clipboard path. */
let lastScreenshotSentAt = 0;

async function pushScreenshotFile(path: string, bytes: number): Promise<void> {
  if (!clipAuto.value || !canSend.value || clipSending) return;
  clipSending = true;
  try {
    const view = await invoke<{ preview: string; byte_len: number }>(
      "session_send_clipboard_image",
      { filePath: path },
    );
    lastScreenshotSentAt = Date.now();
    void refreshHistory();
    clipHint.value = `已自动同步截图（${formatBytes(view.byte_len)}）`;
    pushJob({
      id: `shot-${Date.now()}`,
      kind: "clip",
      title: "自动同步截图",
      detail: basenameOf(path),
      status: "done",
    });
  } catch (e) {
    clipHint.value = `截图同步失败：${String(e).slice(0, 80)}`;
  } finally {
    clipSending = false;
    void bytes;
  }
}

type ViewId = "connect" | "transfer" | "clipboard" | "settings";
const view = ref<ViewId>("connect");
const views: { id: ViewId; label: string }[] = [
  { id: "connect", label: "连接" },
  { id: "transfer", label: "传输" },
  { id: "clipboard", label: "剪贴板" },
  { id: "settings", label: "设置" },
];
const connected = computed(() => connectionLabel.value === "已连接");

interface HistoryEntry {
  id: string;
  kind: "text" | "image";
  preview: string;
  bytes: number;
  ts: number;
}

const history = ref<HistoryEntry[]>([]);

function clock(ts: number): string {
  const d = new Date(ts);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

async function refreshHistory(): Promise<void> {
  try {
    history.value = await invoke<HistoryEntry[]>("clip_history_list", { limit: 20 });
  } catch {
    history.value = [];
  }
}

async function applyHistory(id: string): Promise<void> {
  try {
    await invoke("clip_history_apply", { id });
    clipHint.value = "已把历史条目复制到剪贴板";
  } catch (e) {
    clipHint.value = `复制失败：${String(e).slice(0, 80)}`;
  }
}

async function clearHistory(): Promise<void> {
  try {
    await invoke("clip_history_clear");
    history.value = [];
    clipHint.value = "历史已清空";
  } catch (e) {
    clipHint.value = `清空失败：${String(e).slice(0, 80)}`;
  }
}

let clipTimer: number | null = null;
const shellUnlisteners: UnlistenFn[] = [];

function startClipPoll() {
  stopClipPoll();
  // 400ms + seq gate ≈ near-instant without Win32 HWND listener in WebView.
  clipTimer = window.setInterval(() => void clipPollTick(), 400);
}
function stopClipPoll() {
  if (clipTimer != null) {
    clearInterval(clipTimer);
    clipTimer = null;
  }
}

onMounted(async () => {
  const saved = localStorage.getItem(THEME_KEY);
  if (saved === "light" || saved === "dark" || saved === "system") {
    void applyTheme(saved);
  } else {
    void applyTheme("system");
  }
  await bootstrap();
  void refreshHistory();
  startClipPoll();

  // Tray menu → toggle auto clipboard (M4 Wave A).
  // Screenshot files (PixPin / Snipping Tool) — the most reliable capture source, since many
  // tools never put the bitmap on the clipboard (or only after the user finishes editing).
  shellUnlisteners.push(
    await listen<{ path: string; bytes: number }>("screenshot-file", (e) => {
      void pushScreenshotFile(e.payload.path, e.payload.bytes);
    }),
  );

  shellUnlisteners.push(
    await listen("tray-toggle-clip-auto", () => {
      clipAuto.value = !clipAuto.value;
      clipHint.value = clipAuto.value
        ? "已恢复自动剪贴板同步"
        : "已暂停自动剪贴板同步（托盘可再开）";
      void notifyUser(
        "LanBridge",
        clipAuto.value ? "已恢复自动剪贴板" : "已暂停自动剪贴板",
      );
    }),
  );

  // Window drag-drop → same queue as「添加文件」. Vendor: Window.onDragDropEvent.
  shellUnlisteners.push(
    await getCurrentWindow().onDragDropEvent((event) => {
      if (event.payload.type !== "drop") return;
      for (const path of event.payload.paths) {
        const name = basenameOf(path);
        if (!sendQueue.value.some((f) => f.path === path)) {
          sendQueue.value.push({ path, name });
        }
      }
      if (event.payload.paths.length > 0) {
        clipHint.value = `已拖入 ${event.payload.paths.length} 个文件到发送队列`;
      }
    }),
  );
});

onUnmounted(() => {
  serveAbort = true;
  stopPoll();
  stopClipPoll();
  for (const u of shellUnlisteners) {
    try {
      u();
    } catch {
      /* ignore */
    }
  }
  shellUnlisteners.length = 0;
});
</script>

<template>
  <div class="shell">
    <aside class="side">
      <div class="side-logo">
        <img src="/logo.png" alt="" />
      </div>
      <button
        v-for="v in views"
        :key="v.id"
        type="button"
        class="side-item"
        :class="{ active: view === v.id }"
        @click="view = v.id"
      >
        <span class="side-dot" :class="{ on: v.id === 'connect' && connected }" />
        {{ v.label }}
      </button>
      <div class="side-foot">
        <span class="side-state">{{ shortStateLabel }}</span>
      </div>
    </aside>

    <main class="page">
    <header class="head">
      <div>
        <h1>LanBridge</h1>
        <p class="sub">局域网 · Windows ↔ 鸿蒙</p>
      </div>
      <div class="theme">
        <button
          type="button"
          class="theme-btn"
          :class="{ active: themePref === 'light' }"
          @click="applyTheme('light')"
        >
          浅色
        </button>
        <button
          type="button"
          class="theme-btn"
          :class="{ active: themePref === 'dark' }"
          @click="applyTheme('dark')"
        >
          深色
        </button>
        <button
          type="button"
          class="theme-btn"
          :class="{ active: themePref === 'system' }"
          @click="applyTheme('system')"
        >
          跟系统
        </button>
      </div>
    </header>

    <section class="banner" :class="{ err: !!error }">
      <strong>{{ liveBanner }}</strong>
      <p class="muted">连接：{{ connectionLabel }} · {{ st?.alias }} / {{ st?.device_id }}</p>
      <p v-if="error" class="err-text">{{ error }}</p>
    </section>

    <!-- 1 配对 -->
    <section v-if="view === 'connect'" class="block">
      <!-- 已配对：设备卡 + 两个动作 -->
      <template v-if="st?.paired">
        <h2>已配对设备</h2>
        <div class="peer-card">
          <div class="peer-avatar">📱</div>
          <div class="peer-main">
            <strong>{{ st.peer_alias || st.peer_device_id }}</strong>
            <p class="muted">
              {{ st.peer_host }}<span v-if="st.peer_port">:{{ st.peer_port }}</span>
            </p>
          </div>
          <span class="state-pill" :class="{ on: connectionLabel === '已连接' }">
            {{ connectionLabel }}
          </span>
        </div>
        <div class="row">
          <button type="button" :disabled="busy" @click="probePeer">
            {{ st.listening ? "检测对端" : "重新上线并检测" }}
          </button>
          <button type="button" class="linkish" :disabled="busy" @click="clearPair">
            取消配对
          </button>
        </div>
        <p class="muted">
          取消配对会通知对端：对方立刻显示「对端已取消配对」，之后需重新填码。
        </p>
      </template>

      <!-- 未配对：本机码 / 填对方码 / 附近设备 -->
      <template v-else>
        <h2>连接到手机</h2>
        <p class="muted">
          两台设备连同一个 Wi-Fi（手机开热点给 PC 也算），然后任选一种方式。
        </p>

        <div class="pair-card">
          <p class="muted">① 在手机上填这个码（本机码）</p>
          <p class="code">{{ myCode || "——" }}</p>
          <p v-if="localIp" class="muted">本机 IP {{ localIp }}（附近搜不到时让对方手填）</p>
        </div>

        <div class="pair-card">
          <p class="muted">② 或者在这里填手机屏幕上的 6 位码</p>
          <div class="row">
            <input
              v-model="pairCodeInput"
              class="code-input"
              maxlength="6"
              inputmode="numeric"
              placeholder="6 位码"
              :disabled="busy"
              @keyup.enter="confirmPairWithNearby"
            />
            <button type="button" :disabled="busy" @click="confirmPairWithNearby">
              连接
            </button>
          </div>
        </div>

        <div class="row">
          <button type="button" class="linkish" :disabled="busy" @click="refreshNearby">
            刷新附近
          </button>
          <button type="button" class="linkish" :disabled="busy" @click="toggleManualPair">
            {{ showManualPair ? "收起手填 IP" : "热点下连不上？手填 IP" }}
          </button>
        </div>

        <div v-if="showManualPair" class="manual-pair">
          <p class="muted">
            热点常拦广播：把手机屏上的「本机 IP」填这里，再点「用 IP 配对」。
          </p>
          <label class="field">
            <span>对方 IP</span>
            <input
              v-model="peerHostEdit"
              placeholder="例如 192.168.43.5"
              :disabled="busy"
              @keyup.enter="pairWithManual"
            />
          </label>
          <label class="field">
            <span>端口（默认 37532）</span>
            <input v-model.number="peerPortEdit" type="number" :disabled="busy" />
          </label>
          <button type="button" :disabled="busy" @click="pairWithManual">
            用 IP 配对
          </button>
        </div>

        <h3 class="sub-head">附近的设备</h3>
        <ul class="peer-list">
          <li v-for="p in nearby" :key="p.device_id" class="peer-item">
            <div>
              <strong>{{ p.alias || p.device_id }}</strong>
              <span class="muted"> · {{ p.trusted ? "已配对" : "可配对" }}</span>
            </div>
            <button
              type="button"
              :disabled="busy || pairTargetId === p.device_id"
              @click="pairWithNearby(p)"
            >
              {{ pairTargetId === p.device_id ? "配对中…" : "配对" }}
            </button>
          </li>
          <li v-if="!nearby.length" class="muted">
            附近暂无可配对设备（热点下常见）。用上面的本机码或手填 IP 即可。
          </li>
        </ul>
      </template>
    </section>

    <!-- 2 发送 -->
    <section v-if="view === 'transfer'" class="block">
      <h2>发送</h2>
      <div class="row">
        <button type="button" :disabled="busy || !canSend" @click="pickSendFiles">添加文件</button>
        <button
          type="button"
          :disabled="busy || !canSend || !sendQueue.length"
          @click="sendQueued"
        >
          发送队列（{{ sendQueue.length }}）
        </button>
        <button type="button" class="linkish" :disabled="!sendQueue.length" @click="clearSendQueue">
          清空
        </button>
      </div>
      <ul v-if="sendQueue.length" class="queue">
        <li v-for="(f, i) in sendQueue" :key="i">{{ f.name }}</li>
      </ul>
      <p v-else class="muted">可多选文件；点发送后在下方「任务」看每条进度。</p>
    </section>

    <!-- 任务 -->
    <section v-if="view === 'transfer'" class="block">
      <h2>任务</h2>
      <ul v-if="jobs.length" class="jobs">
        <li v-for="j in jobs" :key="j.id" :class="j.status">
          <button
            type="button"
            class="job-row"
            :disabled="!j.path && j.kind !== 'recv'"
            @click="openJobPath(j)"
          >
            <strong>{{ j.title }}</strong>
            <span class="muted"> · {{ j.detail }}</span>
            <span v-if="j.path || j.kind === 'recv'" class="open-hint">打开</span>
          </button>
        </li>
      </ul>
      <p v-else class="muted">暂无任务</p>
    </section>

    <!-- 3 接收 -->
    <section v-if="view === 'transfer'" class="block">
      <h2>接收</h2>
      <p class="body">保存到：{{ destLabel || "（默认 Downloads\\LanBridge）" }}</p>
      <div class="row">
        <button type="button" :disabled="busy" @click="pickRecvDir">更改目录</button>
        <button type="button" class="linkish" :disabled="!destRoot" @click="openRecvFolder">
          打开目录
        </button>
      </div>
      <p class="muted">已配对连接后自动接收，无需再点「开始听」。</p>
    </section>

    <!-- 4 剪贴板 -->
    <section v-if="view === 'clipboard'" class="block">
      <h2>双向剪贴板</h2>
      <label class="toggle">
        <input v-model="clipAuto" type="checkbox" />
        <span>自动同步到手机（文本）</span>
      </label>
      <p class="muted">{{ clipHint }}</p>
      <button type="button" :disabled="busy || !canSend" @click="pushClipboardOnce">
        立即推送本机剪贴板
      </button>

      <div class="hist-head">
        <h2>最近剪贴板</h2>
        <button type="button" class="linkish" @click="clearHistory">清空</button>
      </div>
      <p class="muted">本机保存（最多 100 条，图片存在本地目录），点击条目可重新复制。</p>
      <ul v-if="history.length > 0" class="hist">
        <li v-for="h in history" :key="h.id" @click="applyHistory(h.id)">
          <span class="hist-kind">{{ h.kind === "image" ? "图" : "文" }}</span>
          <span class="hist-preview">{{ h.preview }}</span>
          <span class="hist-time">{{ clock(h.ts) }}</span>
        </li>
      </ul>
      <p v-else class="muted">还没有记录。</p>
    </section>

    <section v-if="view === 'settings'" class="block">
      <button type="button" class="linkish" @click="showAdvanced = !showAdvanced">
        {{ showAdvanced ? "收起高级" : "高级" }}
      </button>
      <div v-if="showAdvanced">
        <label class="field">
          <span>本机监听端口（0=默认 37532）</span>
          <input v-model.number="listenPortEdit" type="number" :disabled="busy" />
        </label>
        <p class="muted">监听端口兜底（0=默认 37532）。配对请用上方「手填对方 IP」。</p>
      </div>
    </section>
    </main>
  </div>
</template>

<style>
html,
body,
#app {
  height: 100%;
  margin: 0;
  overflow: hidden;
}
.shell {
  display: flex;
  height: 100vh;
  overflow: hidden;
  background: var(--bg);
}
.side {
  flex: none;
  width: 128px;
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 14px 8px;
  background: color-mix(in srgb, var(--fg) 4%, var(--bg));
  border-right: 1px solid var(--border);
}
.side-logo {
  padding: 4px 6px 14px;
}
.side-logo img {
  width: 34px;
  height: 34px;
  border-radius: 9px;
  display: block;
}
.side-item {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 9px 10px;
  border: 0;
  border-radius: 9px;
  background: transparent;
  color: var(--muted);
  font-size: 13px;
  text-align: left;
  cursor: pointer;
}
.side-item:hover {
  background: color-mix(in srgb, var(--fg) 6%, transparent);
}
.side-item.active {
  background: color-mix(in srgb, var(--primary) 12%, transparent);
  color: var(--primary);
  font-weight: 600;
}
.side-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--border);
}
.side-dot.on {
  background: var(--success);
}
.side-foot {
  margin-top: auto;
  padding: 8px 10px;
  font-size: 12px;
  color: var(--muted);
}

:root {
  --bg: #f8f5fc;
  --fg: #221436;
  --muted: #7a6b8e;
  --border: #ede5f6;
  --btn-bg: #6d28d9;
  --btn-fg: #ffffff;
  --primary: #6d28d9;
  --accent: #a78bfa;
  --success: #1f9d6a;
  --danger: #d64545;
  color-scheme: light dark;
  color: var(--fg);
  background: var(--bg);
  font-family: "Segoe UI", "Microsoft YaHei", system-ui, sans-serif;
}

.hist-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 8px;
}
.hist {
  list-style: none;
  margin: 6px 0 0;
  padding: 0;
  max-height: 220px;
  overflow: auto;
  border-top: 1px solid var(--border);
}
.hist li {
  display: flex;
  gap: 8px;
  align-items: baseline;
  padding: 6px 2px;
  border-bottom: 1px solid var(--border);
  cursor: pointer;
}
.hist li:hover {
  background: color-mix(in srgb, var(--fg) 6%, transparent);
}
.hist-kind {
  flex: none;
  width: 18px;
  text-align: center;
  opacity: 0.6;
}
.hist-preview {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.hist-time {
  flex: none;
  font-size: 12px;
  color: var(--muted);
}

@media (prefers-color-scheme: dark) {
  :root:not([data-theme]) {
    --bg: #0b0614;
    --fg: #f1eafb;
    --muted: #b6a6cb;
    --border: #2a1b3f;
    --btn-bg: #a78bfa;
    --btn-fg: #1b0f33;
    --primary: #a78bfa;
    --accent: #d8b4fe;
    --success: #4ade80;
    --danger: #ff7a7a;
  }
}

html[data-theme="light"] {
  --bg: #ffffff;
  --fg: #000000;
  --muted: #666666;
  --border: #dddddd;
  --btn-bg: #000000;
  --btn-fg: #ffffff;
}

html[data-theme="dark"] {
  --bg: #000000;
  --fg: #ffffff;
  --muted: #aaaaaa;
  --border: #333333;
  --btn-bg: #ffffff;
  --btn-fg: #000000;
}

html,
body,
#app {
  margin: 0;
  min-height: 100%;
  background: var(--bg);
  color: var(--fg);
}

.page {
  flex: 1 1 auto;
  width: 100%;
  min-width: 0;
  max-width: none;
  margin: 0;
  padding: 1.2rem 1.5rem 2.5rem;
  box-sizing: border-box;
  overflow-y: auto;
}

.head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 1rem;
  padding-bottom: 1rem;
  border-bottom: 1px solid var(--border);
}

h1 {
  margin: 0;
  font-size: 1.75rem;
  font-weight: 700;
}

.sub {
  margin: 0.35rem 0 0;
  color: var(--muted);
  font-size: 0.9rem;
}

.theme {
  display: flex;
  gap: 0.25rem;
}

.theme-btn {
  margin: 0;
  padding: 0.25rem 0.5rem;
  font-size: 0.8rem;
  background: transparent;
  color: var(--fg);
  border: 1px solid var(--border);
}

.theme-btn.active {
  background: var(--btn-bg);
  color: var(--btn-fg);
  border-color: var(--btn-bg);
}

.banner {
  margin-top: 1rem;
  padding: 0.75rem;
  border: 1px solid var(--border);
}

.banner.err {
  border-color: #c00;
}

.err-text {
  color: #c00;
  margin: 0.35rem 0 0;
  font-size: 0.9rem;
}

.block {
  margin-top: 1.25rem;
  padding-top: 1rem;
  border-top: 1px solid var(--border);
}

h2 {
  margin: 0 0 0.35rem;
  font-size: 1.15rem;
  font-weight: 600;
}

.body {
  margin: 0.25rem 0;
}

.peer-line {
  font-weight: 600;
}

.muted {
  color: var(--muted);
  font-size: 0.9rem;
  margin: 0.25rem 0;
}

.row {
  display: flex;
  flex-wrap: wrap;
  gap: 0.25rem;
  align-items: center;
}

button {
  background: var(--btn-bg);
  color: var(--btn-fg);
  border: 1px solid var(--btn-bg);
  border-radius: 0;
  padding: 0.5rem 0.85rem;
  cursor: pointer;
  margin-right: 0.5rem;
  margin-top: 0.4rem;
  font: inherit;
}

button:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}

button.linkish {
  background: transparent;
  color: var(--fg);
  border: 0;
  text-decoration: underline;
  padding-left: 0;
}

.code-box {
  margin: 0.5rem 0 0.75rem;
}

.manual-pair {
  margin: 0.75rem 0 1rem;
  padding: 0.75rem;
  border: 1px dashed var(--border);
}

.peer-line {
  margin: 0.35rem 0;
  font-weight: 600;
}

.code {
  font-family: ui-monospace, Consolas, monospace;
  font-size: 2rem;
  letter-spacing: 0.22em;
  margin: 0.2rem 0;
  font-variant-numeric: tabular-nums;
}

/* 连接页（产品态）：已配对设备卡 / 配对卡 */
.peer-card {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  padding: 0.85rem 0.75rem;
  border: 1px solid var(--border);
  margin: 0.5rem 0 0.75rem;
}

.peer-avatar {
  font-size: 1.7rem;
  line-height: 1;
}

.peer-main {
  flex: 1;
  min-width: 0;
}

.peer-main strong {
  font-size: 1.05rem;
}

.peer-main p {
  margin: 0.15rem 0 0;
}

.state-pill {
  font-size: 0.8rem;
  padding: 0.2rem 0.55rem;
  border: 1px solid var(--border);
  color: var(--muted);
  white-space: nowrap;
}

.state-pill.on {
  color: var(--accent);
  border-color: var(--accent);
}

.pair-card {
  padding: 0.6rem 0.75rem;
  border: 1px solid var(--border);
  margin: 0.5rem 0;
}

.pair-card .muted {
  margin: 0.1rem 0;
}

.code-input {
  font-family: ui-monospace, Consolas, monospace;
  font-size: 1.05rem;
  letter-spacing: 0.25em;
  width: 9rem;
  padding: 0.45rem 0.5rem;
  border: 1px solid var(--border);
  background: var(--bg);
  color: var(--fg);
  font-variant-numeric: tabular-nums;
}

.sub-head {
  font-size: 0.95rem;
  margin: 1.1rem 0 0.25rem;
}

.peer-list,
.queue,
.jobs {
  list-style: none;
  padding: 0;
  margin: 0.5rem 0 0;
}

.peer-item {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 0.75rem;
  padding: 0.5rem 0;
  border-bottom: 1px solid var(--border);
}

.field {
  display: flex;
  flex-direction: column;
  gap: 0.25rem;
  margin-top: 0.75rem;
}

.field input {
  padding: 0.45rem 0.5rem;
  border: 1px solid var(--border);
  background: var(--bg);
  color: var(--fg);
  font: inherit;
}

.toggle {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  margin: 0.5rem 0;
}

.jobs li.done {
  opacity: 0.85;
}

.jobs li.fail {
  color: #c00;
}

.jobs li.active {
  font-weight: 600;
}

.job-row {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 0.25rem 0.5rem;
  width: 100%;
  text-align: left;
  background: transparent;
  color: inherit;
  border: 0;
  padding: 0.35rem 0;
  font: inherit;
  cursor: pointer;
}

.job-row:disabled {
  cursor: default;
  opacity: 1;
}

.open-hint {
  margin-left: auto;
  font-size: 0.85rem;
  text-decoration: underline;
  color: var(--muted);
}
</style>
