import {
  abilityAccessCtrl,
  common,
  ConfigurationConstant,
  Permissions,
  Want,
  wantAgent,
  WantAgent,
  wantConstant
} from '@kit.AbilityKit';
import { BusinessError, pasteboard } from '@kit.BasicServicesKit';
import { wifiManager } from '@kit.ConnectivityKit';
import { image } from '@kit.ImageKit';
import { fileIo, fileUri, picker } from '@kit.CoreFileKit';
import { dataSharePredicates, uniformTypeDescriptor } from '@kit.ArkData';
import { photoAccessHelper } from '@kit.MediaLibraryKit';
import { window } from '@kit.ArkUI';
import { backgroundTaskManager } from '@kit.BackgroundTasksKit';
import { notificationManager } from '@kit.NotificationKit';
import {
  sessionBeginPair,
  sessionGoOffline,
  sessionGoOnline,
  sessionOpen,
  sessionPair,
  sessionPairingCode,
  sessionProbePeer,
  sessionSendClipboardPng,
  sessionSendClipboardText,
  sessionSendFile,
  sessionServeOnce,
  sessionSetPeer,
  sessionStatus,
  sessionUnpair
} from 'liblb_napi.so';

// 截图开关跨重启保持（必须在 @StorageLink 初始化前建立持久化属性）。
PersistentStorage.persistProp<boolean>('lbShotAuto', false);

type ThemePref = 'light' | 'dark' | 'system';

interface NearbyPeerObj {
  device_id: string;
  alias: string;
  host: string;
  port: number;
  trusted: boolean;
  last_seen_ms: number;
}

interface SessionStatusObj {
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
  /** 'unpaired' | 'timeout' | 'closed' — absent while connected. */
  disconnect_reason?: string | null;
  nearby: NearbyPeerObj[];
  banner: string;
  transfer_done?: number;
  transfer_total?: number;
  transfer_bps?: number;
  transfer_name?: string;
  last_send_probe_bps?: number;
  last_send_probe_elapsed_us?: number;
  last_send_transfer_bps?: number;
  last_send_transfer_elapsed_us?: number;
  last_send_bytes?: number;
}

interface ServeEventObj {
  kind: string;
  bytes?: number;
  path?: string;
  text?: string;
  png_base64?: string;
  message?: string;
}

interface SendQueuedFile {
  path: string;
  name: string;
}

/** Mirrors EntryAbility AppStorage share payload (Wave B). */
interface LbSharePendingPayload {
  texts: string[];
  uris: string[];
  ts: number;
}

interface JobCard {
  id: string;
  kind: 'send' | 'recv' | 'clip';
  title: string;
  detail: string;
  status: 'queued' | 'active' | 'done' | 'fail';
  path?: string;
  /** Bump on every visual update so ArkUI ForEach re-renders (key must change). */
  rev?: number;
}

@Entry
@Component
struct Index {
  @State banner: string = '正在打开会话…';
  @State stateLabel: string = '—';
  @State paired: boolean = false;
  @State listening: boolean = false;
  @State listenPort: number = 37532;
  @State pairingCode: string = '';
  @State deviceLine: string = '';
  @State peerLine: string = '';
  @State localIpLine: string = '';
  @State connectionLabel: string = '未配对';
  /** Core-reported disconnect reason; drives the 三态 label (unpaired/timeout/closed). */
  @State disconnectReason: string = '';
  @State canOperate: boolean = false;
  @State liveBanner: string = '正在打开会话…';
  @State nearbyList: NearbyPeerObj[] = [];
  @State peerHostEdit: string = '';
  @State peerPortEdit: number = 37532;
  @State pairCodeEdit: string = '';
  @State listenPortEdit: number = 0;
  @State showAdvanced: boolean = false;
  @State showManualPair: boolean = false;
  @State showSettings: boolean = false;
  /** User collapsed manual IP — do not auto-reopen on every status poll. */
  @State manualPairUserClosed: boolean = false;
  @State destReady: boolean = false;
  /** User-visible receive destination (DocumentPickerMode.DOWNLOAD dir URI). */
  @State destLabel: string = '';
  @State sendQueue: Array<SendQueuedFile> = [];
  @State jobs: Array<JobCard> = [];
  @State clipText: string = '你好 LanBridge';
  @State clipImageLabel: string = '';
  @State clipPng: ArrayBuffer | null = null;
  @State busy: boolean = false;
  @State error: string = '';
  @State themePref: ThemePref = 'system';
  @State clipHint: string = '配对上线后授权读剪贴板：本机复制即自动同步到电脑';
  private serveLoop: boolean = false;
  private serveAbort: boolean = false;
  private serveWorkers: number = 0;
  /** One accept loop — dual workers steal file data-plane accepts →「入站异常」. */
  private static readonly SERVE_WORKERS: number = 1;
  private destRoot: string = '';
  /** Edge-detect for continuous-task / clip-watch teardown (avoid poll spam). */
  private prevCanOperate: boolean = false;
  /** Throttle wifiManager.getIpInfo — status poll is 250ms. */
  private wifiIpCache: string = '';
  private wifiIpAtMs: number = 0;
  private static readonly WIFI_IP_TTL_MS: number = 2000;
  /** Persisted folder URI for auto-save (DOWNLOAD mode; persistent grant). */
  private recvDestUri: string = '';
  private activeJobId: string = '';
  private lastTransferSnap: SessionStatusObj | null = null;
  private lastClipText: string = '';
  private lastClipPngSig: string = '';
  /** While we are the ones writing the pasteboard, ignore the resulting 'update' event. */
  private clipSelfWriteUntil: number = 0;
  /** Set when an inbound PNG was written; the next read must only be recorded, never echoed. */
  private clipInboundPngPending: boolean = false;
  /** Last image received from the PC; re-applied when the app returns to the foreground. */
  private pendingInboundPng: ArrayBuffer | null = null;
  private pendingInboundAt: number = 0;
  private clipWatching: boolean = false;
  private clipAutoBusy: boolean = false;
  private clipPasteListener: (() => void) | null = null;
  private clipPermAsking: boolean = false;
  /** After user denies, do not re-popup until this time (ms). */
  private clipPermCooldownUntil: number = 0;
  private clipLastCheckMs: number = 0;
  private clipHadGrant: boolean = false;
  @State clipAutoOk: boolean = false;
  /** 截图即共享：监听相册新增的截图并自动发到电脑。 */
  @StorageLink('lbShotAuto') shotAuto: boolean = false;
  @State shotHint: string = '';
  private shotTimer: number = -1;
  private shotWatching: boolean = false;
  /** 一张截图正在发送时忽略后续轮询，避免两次发送在同一会话上交错。 */
  private shotSending: boolean = false;
  private lastShotName: string = '';
  @State bgTaskOk: boolean = false;
  @StorageLink('lbForeground') @Watch('onFgLinkChanged') lbForeground: boolean = true;
  /** Bumped by EntryAbility when system share delivers Want (Share Kit). */
  @StorageLink('lbSharePendingRev') @Watch('onSharePendingChanged') sharePendingRev: number = 0;
  /** Texts from share waiting for pair / idle send. */
  private shareTextsPending: string[] = [];
  private shareFlushBusy: boolean = false;
  private bgTaskStarting: boolean = false;
  private bgTaskRefreshTimer: number = -1;
  /** Refresh continuous task before DATA_TRANSFER idle cancel (~10 min). */
  private static readonly BG_TASK_REFRESH_MS: number = 6 * 60 * 1000;
  /** notificationId from startBackgroundRunning — required for DATA_TRANSFER progress. */
  private bgTaskNotifId: number = -1;
  private notifEnableTried: boolean = false;
  private lastLivePct: number = -1;
  private lastLiveAt: number = 0;
  private static readonly LIVE_PROGRESS_MIN_MS: number = 800;
  private static readonly NOTIF_ID_PAIR: number = 9101;
  private static readonly NOTIF_ID_CLIP: number = 9102;

  aboutToAppear(): void {
    AppStorage.setOrCreate('lbForeground', true);
    AppStorage.setOrCreate('lbClipDirty', false);
    AppStorage.setOrCreate('lbSharePendingJson', '');
    AppStorage.setOrCreate('lbSharePendingRev', 0);
    this.applyTheme('system');
    this.loadSafeArea();
    this.openSession();
    // Cold-start share may land in AppStorage before @StorageLink watches.
    void this.consumeSharePending();
  }

  onPageShow(): void {
    void this.ensureContinuousTask();
    void this.flushClipAfterForeground();
    void this.reapplyInboundClipboard();
    void this.resumeShotWatchIfEnabled();
    void this.consumeSharePending();
  }

  /**
   * The system does not reliably apply pasteboard writes coming from a background app, so the
   * image that arrived while we were in the background is written again once we are in front.
   */
  async reapplyInboundClipboard(): Promise<void> {
    if (this.pendingInboundPng === null) {
      return;
    }
    if (Date.now() - this.pendingInboundAt > 120000) {
      this.pendingInboundPng = null;
      return;
    }
    const png = this.pendingInboundPng;
    this.pendingInboundPng = null;
    const ok = await this.writeOsClipboardPng(png);
    if (ok) {
      this.clipHint = '已把电脑刚发来的图片写入剪贴板';
    }
  }

  onFgLinkChanged(): void {
    if (this.lbForeground) {
      void this.ensureContinuousTask();
      void this.flushClipAfterForeground();
      void this.consumeSharePending();
    }
  }

  onSharePendingChanged(): void {
    void this.consumeSharePending();
  }

  aboutToDisappear(): void {
    this.serveAbort = true;
    this.stopClipWatch();
    this.clearBgTaskRefreshTimer();
    void this.stopContinuousTask();
  }

  parseStatus(json: string): SessionStatusObj {
    return JSON.parse(json) as SessionStatusObj;
  }

  applyStatus(json: string): void {
    const s = this.parseStatus(json);
    this.paired = s.paired;
    this.listening = s.listening;
    const nextCan = s.paired && s.listening;
    this.canOperate = nextCan;
    if (nextCan) {
      void this.ensureClipAutoSilent();
      void this.ensureContinuousTask();
      if (this.destReady) {
        void this.startServeLoop();
      }
      // Share staged while unpaired → send once operable.
      if (!this.prevCanOperate) {
        void this.tryFlushPendingShare();
      }
    } else if (this.prevCanOperate) {
      // Only tear down on paired→unpaired (or listen drop), not every 250ms poll.
      this.stopClipWatch();
      void this.stopContinuousTask();
    }
    this.prevCanOperate = nextCan;
    this.listenPort = s.listen_port;
    if (s.pairing_code !== null && s.pairing_code.length > 0) {
      this.pairingCode = s.pairing_code;
    } else if (s.paired) {
      this.pairingCode = '';
    }
    this.deviceLine = `${s.alias} · ${s.device_id}`;
    this.stateLabel = this.labelOf(s.state);
    this.liveBanner = s.banner.length > 0 ? s.banner : this.banner;
    this.banner = this.liveBanner;
    if (s.banner.indexOf('网络已切换') >= 0) {
      this.manualPairUserClosed = false;
      this.showManualPair = true;
      this.clipHint = '换网后旧对端 IP 失效：可先「重试连接」；失败则清除配对，用手填新 IP';
    }
    this.disconnectReason = s.disconnect_reason !== undefined && s.disconnect_reason !== null ?
      s.disconnect_reason : '';
    this.connectionLabel = this.connectionOf(s.connection);
    this.nearbyList = s.nearby !== undefined && s.nearby !== null ? s.nearby : [];
    if (!s.paired && this.nearbyList.length === 0 && !this.manualPairUserClosed) {
      this.showManualPair = true;
    }
    const wifi = this.localWifiIpv4Cached();
    const fromCore = s.local_ipv4 !== null && s.local_ipv4.length > 0 ? s.local_ipv4 : '';
    this.localIpLine = wifi.length > 0 ? wifi : fromCore;
    if (s.paired && s.peer_host !== null && s.peer_host.length > 0) {
      const port = s.peer_port !== null && s.peer_port > 0 ? s.peer_port : 37532;
      const name = s.peer_alias !== null && s.peer_alias.length > 0 ? s.peer_alias :
        (s.peer_device_id !== null && s.peer_device_id.length > 0 ? s.peer_device_id : '对端');
      this.peerLine = `已信任：${name} · ${s.peer_host}:${port}`;
    } else if (s.peer_device_id !== null && s.peer_device_id.length > 0) {
      this.peerLine = `信任对端：${s.peer_device_id}`;
    } else {
      this.peerLine = '';
    }
    if (s.peer_host !== null && s.peer_host.length > 0 && this.peerHostEdit.length === 0) {
      this.peerHostEdit = s.peer_host;
    }
    if (s.peer_port !== null && s.peer_port > 0) {
      this.peerPortEdit = s.peer_port;
    }
  }

  /** Session usable: connected or offline-but-trusted (TCP may still work). */
  linkOk(): boolean {
    return this.connectionLabel === '已连接' ||
      this.connectionLabel === '对端离线';
  }

  connectionOf(c: string): string {
    if (c === 'connected') {
      return '已连接';
    }
    if (c === 'pairing' || c === 'searching') {
      return '等待配对';
    }
    if (this.disconnectReason === 'unpaired') {
      return '对端已取消配对';
    }
    // Reachability only — trust alone never reads as「已连接」.
    if (this.paired && this.listening) {
      return '对端离线';
    }
    if (this.paired) {
      return '本机未上线';
    }
    return '未配对';
  }

  /** 三态文案（与 PC 端同口径）：已连接 / 对端离线 / 未配对。 */
  connectionStateLine(): string {
    if (this.connectionLabel === '已连接') {
      return '已连接 · 可收发';
    }
    if (this.disconnectReason === 'unpaired') {
      return '对端已取消配对 · 请重新配对';
    }
    if (this.disconnectReason === 'timeout') {
      return '对端离线 · 心跳超时，将自动重试';
    }
    return '对端离线 · 将自动重试';
  }

  labelOf(state: string): string {
    if (state === 'unpaired') {
      return '未连接';
    }
    if (state === 'paired_offline') {
      return '本机未上线';
    }
    if (state === 'online') {
      return this.paired ? '在线 · 可收发' : '已上线 · 未配对（须等待配对）';
    }
    if (state === 'pairing') {
      return '等待配对中…';
    }
    if (state === 'transferring_in') {
      return '接收中…';
    }
    if (state === 'transferring_out') {
      return '发送中…';
    }
    return state;
  }

  humanizeNativeError(e: Object | string): string {
    const s = String(e);
    if (s.indexOf('port_in_use') >= 0) {
      return '端口被占用：已自动重试。若仍失败请划掉后台后重开本应用（手机极少见）';
    }
    if (s.indexOf('pair_timeout') >= 0 || s.indexOf('timeout') >= 0) {
      return '配对/等待超时';
    }
    if (s.indexOf('not_open') >= 0 || s.indexOf('rc=40') >= 0) {
      return '会话未就绪，请稍候再试（勿连点）';
    }
    if (s.indexOf('busy') >= 0 || s.indexOf('rc=45') >= 0) {
      return '忙：正在收/发，已自动重试';
    }
    if (s.indexOf('peer_unreachable') >= 0) {
      // UDP "已连接" ≠ 对端此刻在 accept；忙于发送时会停接收。
      if (this.linkOk()) {
        return '对端未接受连接（可能正忙于发送），请稍后再试';
      }
      return '连不上对端，确认对方已上线';
    }
    if (s.indexOf('close_notify') >= 0 || s.indexOf('unexpected eof') >= 0 ||
      s.indexOf('UnexpectedEof') >= 0 || s.indexOf('peer closed') >= 0 ||
      s.indexOf('connection closed') >= 0 || s.indexOf('Connection reset') >= 0 ||
      s.indexOf('强迫关闭') >= 0 || s.indexOf('10054') >= 0) {
      return '';
    }
    return s;
  }

  isSoftPeerClose(s: string): boolean {
    return s.indexOf('close_notify') >= 0 || s.indexOf('unexpected eof') >= 0 ||
      s.indexOf('UnexpectedEof') >= 0 || s.indexOf('peer closed') >= 0 ||
      s.indexOf('connection closed') >= 0 || s.indexOf('Connection reset') >= 0 ||
      s.indexOf('强迫关闭') >= 0 || s.indexOf('10054') >= 0 ||
      s.indexOf('forcibly closed') >= 0;
  }

  hostCtx(): common.UIAbilityContext {
    return this.getUIContext().getHostContext() as common.UIAbilityContext;
  }

  sessionDataDir(): string {
    return `${this.hostCtx().filesDir}/lb-session/device`;
  }

  recvDestConfigPath(): string {
    return `${this.hostCtx().filesDir}/lb-session/recv-dest.json`;
  }

  stageRoot(): string {
    return `${this.hostCtx().cacheDir}/lb-stage`;
  }

  recvStagingRoot(): string {
    return `${this.hostCtx().cacheDir}/lb-recv`;
  }

  labelFromUri(uri: string): string {
    const q = uri.split('?')[0];
    const parts = q.split('/');
    let last = '';
    for (let i = parts.length - 1; i >= 0; i--) {
      if (parts[i].length > 0) {
        last = parts[i];
        break;
      }
    }
    if (last.length === 0) {
      return '已选目录';
    }
    try {
      return decodeURIComponent(last);
    } catch (_e) {
      return last;
    }
  }

  loadRecvDest(): void {
    try {
      const p = this.recvDestConfigPath();
      if (!fileIo.accessSync(p)) {
        this.recvDestUri = '';
        this.destLabel = '';
        return;
      }
      const f = fileIo.openSync(p, fileIo.OpenMode.READ_ONLY);
      try {
        const st = fileIo.statSync(f.fd);
        if (st.size <= 0 || st.size > 8192) {
          return;
        }
        const buf = new ArrayBuffer(st.size);
        fileIo.readSync(f.fd, buf);
        const u8 = new Uint8Array(buf);
        let raw = '';
        for (let i = 0; i < u8.length; i++) {
          raw += String.fromCharCode(u8[i]);
        }
        const obj = JSON.parse(raw) as Record<string, string>;
        const uri = obj['uri'] !== undefined ? obj['uri'] : '';
        if (uri.length > 0) {
          this.recvDestUri = uri;
          this.destLabel = this.labelFromUri(uri);
          try {
            this.verifyRecvDestWritable();
          } catch (_e) {
            // Stale FOLDER / expired grant — force DOWNLOAD re-pick.
            this.recvDestUri = '';
            this.destLabel = '';
          }
        }
      } finally {
        fileIo.closeSync(f);
      }
    } catch (_e) {
      this.recvDestUri = '';
      this.destLabel = '';
    }
  }

  persistRecvDest(): void {
    try {
      const parent = `${this.hostCtx().filesDir}/lb-session`;
      this.ensureDir(parent);
      // URI is ASCII file://…; label re-derived on load (avoid UTF-8 hand encode).
      const body = `{"uri":${JSON.stringify(this.recvDestUri)}}`;
      const p = this.recvDestConfigPath();
      const f = fileIo.openSync(p,
        fileIo.OpenMode.READ_WRITE | fileIo.OpenMode.CREATE | fileIo.OpenMode.TRUNC);
      try {
        const bytes = new Uint8Array(body.length);
        for (let i = 0; i < body.length; i++) {
          bytes[i] = body.charCodeAt(i);
        }
        fileIo.writeSync(f.fd, bytes.buffer);
      } finally {
        fileIo.closeSync(f);
      }
    } catch (_e) {
      // best-effort
    }
  }

  /**
   * Receive folder:
   * - Default (first receive / online): DocumentPickerMode.DOWNLOAD
   *   (Download/<bundle>/; often no UI — system assigns the dir).
   * - 「更改位置」: DocumentSelectMode.FOLDER (shows folder picker).
   * API: @ohos.file.picker (DocumentViewPicker) + fileIo.
   */
  async ensureRecvDest(forcePick: boolean): Promise<boolean> {
    if (!forcePick && this.recvDestUri.length > 0) {
      return true;
    }
    if (forcePick) {
      return await this.pickRecvDestFolder();
    }
    try {
      const saveOpts = new picker.DocumentSaveOptions();
      saveOpts.pickerMode = picker.DocumentPickerMode.DOWNLOAD;
      const documentPicker = new picker.DocumentViewPicker(this.hostCtx());
      const uris: Array<string> = await documentPicker.save(saveOpts);
      if (!uris || uris.length === 0 || uris[0].length === 0) {
        this.banner = '未设置保存目录';
        return false;
      }
      this.recvDestUri = uris[0];
      this.destLabel = this.labelFromUri(uris[0]);
      this.verifyRecvDestWritable();
      this.persistRecvDest();
      return true;
    } catch (e) {
      this.recvDestUri = '';
      this.destLabel = '';
      this.banner = `设置保存位置失败：${String(e)}`;
      this.error = String(e);
      return false;
    }
  }

  /** User-facing folder picker for 「更改位置」. */
  async pickRecvDestFolder(): Promise<boolean> {
    this.banner = '请选择保存文件夹…';
    this.error = '';
    try {
      const opts = new picker.DocumentSelectOptions();
      opts.selectMode = picker.DocumentSelectMode.FOLDER;
      opts.maxSelectNumber = 1;
      const documentPicker = new picker.DocumentViewPicker(this.hostCtx());
      const uris: Array<string> = await documentPicker.select(opts);
      if (!uris || uris.length === 0 || uris[0].length === 0) {
        this.banner = '未更改保存位置';
        return false;
      }
      const prevUri = this.recvDestUri;
      const prevLabel = this.destLabel;
      this.recvDestUri = uris[0];
      this.destLabel = this.labelFromUri(uris[0]);
      try {
        this.verifyRecvDestWritable();
      } catch (ve) {
        this.recvDestUri = prevUri;
        this.destLabel = prevLabel;
        throw new Error(String(ve));
      }
      this.persistRecvDest();
      return true;
    } catch (e) {
      this.banner = `更改位置失败：${String(e)}`;
      this.error = String(e);
      return false;
    }
  }

  /** Join DOWNLOAD dir URI + file name (avoid double slash). */
  joinRecvDestUri(dirUri: string, fileName: string): string {
    const base = dirUri.endsWith('/') ? dirUri.substring(0, dirUri.length - 1) : dirUri;
    return `${base}/${fileName}`;
  }

  /**
   * Create/open a file under DOWNLOAD dir URI.
   * Vendor save-user-file DOWNLOAD: openSync(FileUri(dirUri + '/' + name).path), not the raw URI.
   */
  openCreateUnderRecvDest(fileUriStr: string): fileIo.File {
    let sandPath = '';
    try {
      sandPath = new fileUri.FileUri(fileUriStr).path;
    } catch (_e) {
      sandPath = '';
    }
    if (sandPath.length > 0) {
      const slash = sandPath.lastIndexOf('/');
      if (slash > 0) {
        this.ensureDir(sandPath.substring(0, slash));
      }
      return fileIo.openSync(
        sandPath,
        fileIo.OpenMode.READ_WRITE | fileIo.OpenMode.CREATE | fileIo.OpenMode.TRUNC
      );
    }
    return fileIo.openSync(
      fileUriStr,
      fileIo.OpenMode.READ_WRITE | fileIo.OpenMode.CREATE | fileIo.OpenMode.TRUNC
    );
  }

  /** Fail fast if the chosen DOWNLOAD dir cannot create files. */
  verifyRecvDestWritable(): void {
    if (this.recvDestUri.length === 0) {
      throw new Error('未设置保存位置');
    }
    // No leading '.' — some DocumentFS implementations reject hidden names.
    const probeUri = this.joinRecvDestUri(this.recvDestUri, `lb_write_probe_${Date.now()}.tmp`);
    const dst = this.openCreateUnderRecvDest(probeUri);
    try {
      const one = new Uint8Array(1);
      one[0] = 1;
      fileIo.writeSync(dst.fd, one.buffer);
    } finally {
      fileIo.closeSync(dst);
    }
    try {
      const p = new fileUri.FileUri(probeUri).path;
      if (p.length > 0) {
        fileIo.unlinkSync(p);
      }
    } catch (_e) {
      // probe file left behind is harmless
    }
  }

  async onPickRecvDest(): Promise<void> {
    this.error = '';
    this.banner = '请选择保存目录…';
    try {
      const ok = await this.ensureRecvDest(true);
      if (ok) {
        this.banner = `接收目录已就绪（可写入）：${this.destLabel}`;
        this.error = '';
      } else if (this.banner.indexOf('请选择') === 0 || this.banner.length === 0) {
        this.banner = '未更改保存位置';
      }
    } catch (e) {
      this.banner = `更改位置失败：${String(e)}`;
      this.error = String(e);
    }
  }

  safeFileName(name: string): string {
    let s = name.trim();
    if (s.length === 0) {
      return 'recv.bin';
    }
    s = s.replace(/[\\/:*?"<>|]/g, '_');
    if (s === '.' || s === '..') {
      return 'recv.bin';
    }
    return s;
  }

  /** Unique name under dest dir — never silent overwrite (ADR-001). */
  uniqueDestFileUri(dirUri: string, displayName: string): string {
    const base = this.safeFileName(displayName);
    const dot = base.lastIndexOf('.');
    const stem = dot > 0 ? base.substring(0, dot) : base;
    const ext = dot > 0 ? base.substring(dot) : '';
    for (let n = 0; n < 200; n++) {
      const candidate = n === 0 ? base : `${stem}_${n}${ext}`;
      const joined = this.joinRecvDestUri(dirUri, candidate);
      let path = '';
      try {
        path = new fileUri.FileUri(joined).path;
      } catch (_e) {
        return joined;
      }
      let exists = false;
      try {
        exists = path.length > 0 && fileIo.accessSync(path);
      } catch (_e2) {
        exists = false;
      }
      if (!exists) {
        return joined;
      }
    }
    return this.joinRecvDestUri(dirUri, `${stem}_${Date.now()}${ext}`);
  }

  /** Copy sandbox file into DOWNLOAD dest dir; returns file URI for open. */
  copyPathIntoRecvDest(srcPath: string, displayName: string): string {
    if (this.recvDestUri.length === 0) {
      throw new Error('未设置保存位置');
    }
    const fileUriStr = this.uniqueDestFileUri(this.recvDestUri, displayName);
    const src = fileIo.openSync(srcPath, fileIo.OpenMode.READ_ONLY);
    try {
      const st = fileIo.statSync(src.fd);
      const dst = this.openCreateUnderRecvDest(fileUriStr);
      try {
        this.copyFdFully(src.fd, dst.fd, st.size);
        const written = fileIo.statSync(dst.fd);
        if (written.size !== st.size) {
          throw new Error(`写入不完整：${written.size}/${st.size}`);
        }
      } finally {
        fileIo.closeSync(dst);
      }
    } finally {
      fileIo.closeSync(src);
    }
    return fileUriStr;
  }

  ensureDir(path: string): void {
    try {
      fileIo.mkdirSync(path, true);
    } catch (_e) {
      // exists
    }
  }

  localWifiIpv4Cached(): string {
    const now = Date.now();
    if (this.wifiIpCache.length > 0 && now - this.wifiIpAtMs < Index.WIFI_IP_TTL_MS) {
      return this.wifiIpCache;
    }
    const fresh = this.localWifiIpv4();
    if (fresh.length > 0 && !this.isVirtualLanIp(fresh)) {
      // Net switch: drop stale cache immediately when WLAN IP changes.
      if (this.wifiIpCache.length > 0 && this.wifiIpCache !== fresh) {
        this.manualPairUserClosed = false;
      }
      this.wifiIpCache = fresh;
      this.wifiIpAtMs = now;
      return fresh;
    }
    // Empty / virtual after switch — do not keep old router IP for 30s.
    this.wifiIpCache = '';
    this.wifiIpAtMs = now;
    return '';
  }

  isVirtualLanIp(ip: string): boolean {
    // Harmony anconwlan / Docker-style; keep wlan0 192.168.* for hand-fill.
    return ip.indexOf('172.17.') === 0;
  }

  localWifiIpv4(): string {
    try {
      const n: number = wifiManager.getIpInfo().ipAddress;
      if (!n) {
        return '';
      }
      const s =
        `${(n >>> 24) & 0xff}.${(n >>> 16) & 0xff}.${(n >>> 8) & 0xff}.${n & 0xff}`;
      if (this.isVirtualLanIp(s)) {
        return '';
      }
      return s;
    } catch (_e) {
      return '';
    }
  }

  applyTheme(pref: ThemePref): void {
    this.themePref = pref;
    try {
      const ctx = this.hostCtx();
      if (pref === 'light') {
        ctx.getApplicationContext().setColorMode(ConfigurationConstant.ColorMode.COLOR_MODE_LIGHT);
      } else if (pref === 'dark') {
        ctx.getApplicationContext().setColorMode(ConfigurationConstant.ColorMode.COLOR_MODE_DARK);
      } else {
        ctx.getApplicationContext().setColorMode(ConfigurationConstant.ColorMode.COLOR_MODE_NOT_SET);
      }
    } catch (_e) {
      // ignore
    }
  }

  openSession(): void {
    try {
      this.ensureDir(this.sessionDataDir());
      this.loadRecvDest();
      const json = sessionOpen(this.sessionDataDir(), 'lanbridge-ohos', 'Phone');
      this.applyStatus(json);
      this.banner = this.stateLabel;
      this.error = '';
      void this.onGoOnline();
      this.startStatusPoll();
    } catch (e) {
      this.error = this.humanizeNativeError(e);
    }
  }

  private statusPoll: number = -1;
  private statusPolling: boolean = false;
  private autoProbeBusy: boolean = false;
  private lastAutoProbeMs: number = 0;

  pushJob(job: JobCard): void {
    const withRev: JobCard = {
      id: job.id,
      kind: job.kind,
      title: job.title,
      detail: job.detail,
      status: job.status,
      path: job.path,
      rev: (job.rev !== undefined && job.rev > 0) ? job.rev : 1
    };
    const next: Array<JobCard> = [withRev];
    for (let i = 0; i < this.jobs.length && i < 11; i++) {
      next.push(this.jobs[i]);
    }
    this.jobs = next;
  }

  updateJob(id: string, status: 'queued' | 'active' | 'done' | 'fail', detail: string,
    path?: string, title?: string): void {
    const next: Array<JobCard> = [];
    for (let i = 0; i < this.jobs.length; i++) {
      const j = this.jobs[i];
      if (j.id === id) {
        const prevRev = j.rev !== undefined ? j.rev : 0;
        next.push({
          id: j.id,
          kind: j.kind,
          title: title !== undefined && title.length > 0 ? title : j.title,
          detail: detail,
          status: status,
          path: path !== undefined ? path : j.path,
          rev: prevRev + 1
        });
      } else {
        next.push(j);
      }
    }
    this.jobs = next;
  }

  formatBytes(n: number): string {
    if (n !== n || n < 0) {
      return '—';
    }
    if (n < 1024) {
      return `${Math.round(n)} B`;
    }
    const units: string[] = ['KB', 'MB', 'GB', 'TB'];
    let v = n / 1024;
    let i = 0;
    while (v >= 1024 && i < units.length - 1) {
      v /= 1024;
      i++;
    }
    const text = v < 10 ? v.toFixed(2) : (v < 100 ? v.toFixed(1) : `${Math.round(v)}`);
    return `${text} ${units[i]}`;
  }

  progressDetail(s: SessionStatusObj, fallback: string): string {
    const total = s.transfer_total !== undefined ? s.transfer_total : 0;
    const done = s.transfer_done !== undefined ? s.transfer_done : 0;
    const bps = s.transfer_bps !== undefined ? s.transfer_bps : 0;
    if (total <= 0 && done <= 0) {
      return fallback;
    }
    if (total <= 0) {
      return `${this.formatBytes(done)} 已收发`;
    }
    const pct = Math.min(100, Math.round((done / total) * 100));
    const rate = bps > 0 ? ` · ${this.formatBytes(bps)}/s` : '';
    const phase = done === 0 ? '准备中' : `${pct}%`;
    return `${phase} · ${this.formatBytes(done)} / ${this.formatBytes(total)}${rate}`;
  }

  applyTransferProgress(s: SessionStatusObj): void {
    this.lastTransferSnap = s;
    const total = s.transfer_total !== undefined ? s.transfer_total : 0;
    const done = s.transfer_done !== undefined ? s.transfer_done : 0;
    // Prefer explicit state; also accept live counters if state lagging.
    let liveOut = s.state === 'transferring_out';
    let liveIn = s.state === 'transferring_in';
    if (!liveOut && !liveIn && (total > 0 || done > 0)) {
      if (this.activeJobId.indexOf('send-') === 0 || this.busy) {
        liveOut = true;
      } else {
        liveIn = true;
      }
    }
    if (!liveOut && !liveIn) {
      return;
    }
    if (liveIn && !liveOut) {
      const title = (s.transfer_name !== undefined && s.transfer_name.length > 0) ?
        s.transfer_name : '接收中…';
      const detail = this.progressDetail(s, '接收中…');
      if (this.activeJobId.length === 0 || this.activeJobId.indexOf('recv-live-') !== 0) {
        this.activeJobId = `recv-live-${Date.now()}`;
        this.pushJob({
          id: this.activeJobId,
          kind: 'recv',
          title: title,
          detail: detail,
          status: 'active',
          rev: 1
        });
      } else {
        this.updateJob(this.activeJobId, 'active', detail, undefined, title);
      }
      this.banner = `${title} · ${detail}`;
    } else if (liveOut) {
      // Sender progress: refresh % / rate on the active send card.
      let id = this.activeJobId;
      let foundActive = false;
      let foundDone = false;
      if (id.length > 0) {
        for (let i = 0; i < this.jobs.length; i++) {
          if (this.jobs[i].id === id) {
            if (this.jobs[i].status === 'active') {
              foundActive = true;
            } else if (this.jobs[i].status === 'done') {
              foundDone = true;
            }
            break;
          }
        }
      }
      if (foundDone) {
        return;
      }
      if (!foundActive) {
        for (let i = 0; i < this.jobs.length; i++) {
          if (this.jobs[i].kind === 'send' && this.jobs[i].status === 'active') {
            this.activeJobId = this.jobs[i].id;
            foundActive = true;
            break;
          }
        }
      }
      const useId = this.activeJobId.length > 0 ? this.activeJobId : id;
      // Keep「发送中 i/n」so batch position still moves with %/rate.
      let batchPrefix = '';
      if (useId.length > 0) {
        for (let i = 0; i < this.jobs.length; i++) {
          if (this.jobs[i].id === useId) {
            const m = this.jobs[i].detail.match(/^发送中\s+\d+\/\d+/);
            if (m !== null) {
              batchPrefix = m[0];
            }
            break;
          }
        }
      }
      const core = (total > 0 && done >= total) ?
        this.progressDetail(s, '对端确认中…') : this.progressDetail(s, '发送中…');
      const detail = batchPrefix.length > 0 ? `${batchPrefix} · ${core}` : core;
      const title = (s.transfer_name !== undefined && s.transfer_name.length > 0) ?
        s.transfer_name : undefined;
      if (!foundActive) {
        const name = title !== undefined ? title : '发送中…';
        id = `send-live-${Date.now()}`;
        this.activeJobId = id;
        this.pushJob({
          id: id,
          kind: 'send',
          title: name,
          detail: detail,
          status: 'active',
          rev: 1
        });
      } else {
        this.updateJob(useId, 'active', detail, undefined, title);
      }
      this.banner = `发送中 · ${detail}`;
    }
    this.maybePublishTransferLive(s);
  }

  /** DATA_TRANSFER progress must use system live view + downloadTemplate (vendor). */
  private maybePublishTransferLive(s: SessionStatusObj): void {
    if (this.bgTaskNotifId < 0) {
      return;
    }
    const total = s.transfer_total !== undefined ? s.transfer_total : 0;
    const done = s.transfer_done !== undefined ? s.transfer_done : 0;
    if (total <= 0) {
      return;
    }
    const pct = Math.min(100, Math.round((done / total) * 100));
    const now = Date.now();
    if (pct !== 100 && pct === this.lastLivePct) {
      return;
    }
    if (pct !== 100 && (now - this.lastLiveAt) < Index.LIVE_PROGRESS_MIN_MS) {
      return;
    }
    this.lastLivePct = pct;
    this.lastLiveAt = now;
    const name = (s.transfer_name !== undefined && s.transfer_name.length > 0) ?
      s.transfer_name : '传输中';
    const title = s.state === 'transferring_in' ? '接收中' : '发送中';
    void this.publishTransferLiveProgress(pct, title, name);
  }

  private async ensureNotificationEnabled(): Promise<boolean> {
    try {
      if (notificationManager.isNotificationEnabledSync()) {
        return true;
      }
    } catch (_e) {
      // fall through
    }
    if (this.notifEnableTried) {
      return false;
    }
    this.notifEnableTried = true;
    try {
      await notificationManager.requestEnableNotification(this.hostCtx());
      return notificationManager.isNotificationEnabledSync();
    } catch (_e) {
      return false;
    }
  }

  /** Ordinary text alert — no clipboard body / file content. */
  private async notifyAlert(id: number, title: string, text: string): Promise<void> {
    if (!(await this.ensureNotificationEnabled())) {
      return;
    }
    try {
      const request: notificationManager.NotificationRequest = {
        id: id,
        content: {
          notificationContentType: notificationManager.ContentType.NOTIFICATION_CONTENT_BASIC_TEXT,
          normal: {
            title: title,
            text: text
          }
        }
      };
      await notificationManager.publish(request);
    } catch (_e) {
      // best-effort
    }
  }

  private async publishTransferLiveProgress(pct: number, title: string, fileName: string): Promise<void> {
    if (this.bgTaskNotifId < 0) {
      return;
    }
    if (!(await this.ensureNotificationEnabled())) {
      return;
    }
    try {
      const downLoadTemplate: notificationManager.NotificationTemplate = {
        name: 'downloadTemplate',
        data: {
          title: title,
          fileName: fileName,
          progressValue: pct
        }
      };
      const request: notificationManager.NotificationRequest = {
        content: {
          notificationContentType: notificationManager.ContentType.NOTIFICATION_CONTENT_SYSTEM_LIVE_VIEW,
          systemLiveView: {
            typeCode: 8,
            title: title,
            text: `${pct}%`
          }
        },
        id: this.bgTaskNotifId,
        notificationSlotType: notificationManager.SlotType.LIVE_VIEW,
        template: downLoadTemplate
      };
      await notificationManager.publish(request);
    } catch (_e) {
      // best-effort; do not break transfer UX
    }
  }

  /** Extension to UTD via getUniformDataTypeByFilenameExtension. Never use star-slash MIME. */
  utdForFileName(nameOrPath: string): string {
    const base = nameOrPath.indexOf('/') >= 0 ?
      nameOrPath.substring(nameOrPath.lastIndexOf('/') + 1) : nameOrPath;
    const clean = base.split('?')[0];
    const dot = clean.lastIndexOf('.');
    if (dot < 0 || dot >= clean.length - 1) {
      return '';
    }
    const ext = `.${clean.substring(dot + 1).toLowerCase()}`;
    try {
      return uniformTypeDescriptor.getUniformDataTypeByFilenameExtension(ext);
    } catch (_e) {
      if (ext === '.mp4' || ext === '.mov' || ext === '.mkv' || ext === '.webm' ||
        ext === '.avi' || ext === '.m4v') {
        return 'general.video';
      }
      if (ext === '.jpg' || ext === '.jpeg' || ext === '.png' || ext === '.gif' ||
        ext === '.webp' || ext === '.bmp' || ext === '.heic') {
        return 'general.image';
      }
      if (ext === '.mp3' || ext === '.aac' || ext === '.wav' || ext === '.flac' ||
        ext === '.m4a' || ext === '.ogg') {
        return 'general.audio';
      }
      if (ext === '.pdf') {
        return 'com.adobe.pdf';
      }
      if (ext === '.txt' || ext === '.md' || ext === '.log' || ext === '.csv') {
        return 'general.plain-text';
      }
      return '';
    }
  }

  resolveViewUri(pathOrUri: string): string {
    if (pathOrUri.indexOf('://') >= 0) {
      return pathOrUri;
    }
    return fileUri.getUriFromPath(pathOrUri);
  }

  async startViewData(uri: string, type: string, forcePicker: boolean): Promise<void> {
    const flags = wantConstant.Flags.FLAG_AUTH_READ_URI_PERMISSION |
      wantConstant.Flags.FLAG_AUTH_WRITE_URI_PERMISSION;
    const want: Want = {
      action: 'ohos.want.action.viewData',
      uri: uri,
      flags: flags
    };
    if (type.length > 0) {
      want.type = type;
    }
    if (forcePicker) {
      want.parameters = {
        'ohos.ability.params.showDefaultPicker': true
      };
    }
    await this.hostCtx().startAbility(want);
  }

  async openJob(j: JobCard): Promise<void> {
    if (j.path === undefined || j.path.length === 0) {
      this.banner = '无本地路径可打开';
      return;
    }
    const nameHint = (j.title !== undefined && j.title.length > 0) ? j.title : j.path;
    const type = this.utdForFileName(nameHint.length > 0 ? nameHint : j.path);
    let uri = this.resolveViewUri(j.path);
    try {
      if (j.path.indexOf('://') < 0) {
        let exists = false;
        try {
          exists = fileIo.accessSync(j.path);
        } catch (_miss) {
          exists = false;
        }
        if (!exists) {
          throw new Error('本地文件不存在（可能已被清理）');
        }
      }
      await this.startViewData(uri, type, false);
      this.error = '';
      return;
    } catch (e1) {
      try {
        await this.startViewData(uri, type, true);
        this.error = '';
        return;
      } catch (e2) {
        // Recv still in sandbox: export to user location then open that URI.
        if (j.kind === 'recv' && j.path.indexOf('://') < 0) {
          try {
            const savedUri = await this.exportFileToUser(j.path,
              nameHint.length > 0 ? this.basenameFromUri(nameHint) : 'recv.bin');
            this.updateJob(j.id, 'done',
              j.detail.indexOf('已保存') >= 0 ? j.detail : `已保存 · ${j.detail}`,
              savedUri, j.title);
            uri = savedUri;
            const t2 = this.utdForFileName(nameHint);
            await this.startViewData(uri, t2, true);
            this.banner = `已打开：${j.title}`;
            this.error = '';
            return;
          } catch (e3) {
            const msg = `打开失败：${String(e3)}`;
            this.banner = msg;
            this.error = msg;
            return;
          }
        }
        const msg = `打开失败：${String(e2)}`;
        this.banner = msg;
        this.error = msg;
      }
    }
  }

  /** Auto-save into default DOWNLOAD dir; fall back to one-shot save picker. */
  async promptSaveRecvJob(jobId: string, localPath: string, displayName: string,
    bytes: number): Promise<void> {
    const title = displayName.length > 0 ? displayName : 'recv.bin';
    try {
      if (this.recvDestUri.length === 0) {
        await this.ensureRecvDest(false);
      }
      if (this.recvDestUri.length > 0) {
        const savedUri = this.copyPathIntoRecvDest(localPath, title);
        this.updateJob(jobId, 'done', `已保存 · ${this.formatBytes(bytes)}`, savedUri, title);
        this.banner = `已保存到 ${this.destLabel} · ${title}`;
        this.error = '';
        return;
      }
      const savedUri = await this.exportFileToUser(localPath, title);
      this.updateJob(jobId, 'done', `已保存 · ${this.formatBytes(bytes)}`, savedUri, title);
      this.banner = `已保存：${title}`;
      this.error = '';
    } catch (ex) {
      const soft = String(ex);
      this.banner = soft.indexOf('未选择') >= 0 || soft.indexOf('未设置') >= 0 ?
        `已落盘缓存，请点「更改位置」或「打开文件」· ${title}` :
        `已落盘缓存，保存未完成：${soft}`;
    }
  }

  async refreshStatusKeepBanner(): Promise<void> {
    try {
      const json = await sessionStatus();
      const prevBanner = this.banner;
      const prevError = this.error;
      this.applyStatus(json);
      if (this.busy) {
        this.banner = prevBanner;
        this.error = prevError;
      }
    } catch (_e) {
      // ignore
    }
  }

  startStatusPoll(): void {
    if (this.statusPoll >= 0) {
      return;
    }
    this.statusPoll = setInterval(() => {
      void this.pollTick();
    }, 250) as number;
  }

  async pollTick(): Promise<void> {
    if (this.statusPolling) {
      return;
    }
    this.statusPolling = true;
    try {
      const prev = this.paired;
      // sessionStatus is sync (try_lock + live atomics) — must not queue behind send/serve.
      const json = sessionStatus();
      const s = this.parseStatus(json);
      if (!this.busy) {
        this.applyStatus(json);
        if (this.paired && !prev) {
          void this.notifyAlert(Index.NOTIF_ID_PAIR, 'LanBridge', '已配对');
          void this.startServeLoop();
        }
        if (this.listening && !this.paired && this.connectionLabel !== '等待配对') {
          void this.runBeginPairWait();
        }
        void this.maybeAutoProbe();
      }
      this.applyTransferProgress(s);
    } catch (_e) {
      // ignore
    } finally {
      this.statusPolling = false;
    }
  }

  /** TLS probe; while「已连接」仍约 5s 复检，避免单边信任假在线. */
  async maybeAutoProbe(): Promise<void> {
    if (!this.paired || !this.listening || this.busy || this.autoProbeBusy) {
      return;
    }
    if (this.connectionLabel === '等待配对') {
      return;
    }
    const now = Date.now();
    const interval = this.connectionLabel === '已连接' ? 20000 : 4000;
    if (now - this.lastAutoProbeMs < interval) {
      return;
    }
    this.lastAutoProbeMs = now;
    this.autoProbeBusy = true;
    try {
      const json = await sessionProbePeer();
      this.applyStatus(json);
    } catch (_e) {
      // Core expires session-live on fail — refresh so UI drops「已连接」.
      try {
        const json = sessionStatus();
        this.applyStatus(json);
      } catch (_e2) {
        // ignore
      }
    } finally {
      this.autoProbeBusy = false;
    }
  }

  async onGoOnline(): Promise<void> {
    if (this.listening) {
      return;
    }
    this.busy = true;
    this.error = '';
    this.banner = '上线中…';
    try {
      const port = this.listenPortEdit > 0 ? this.listenPortEdit : 0;
      let json = await sessionGoOnline(port);
      this.applyStatus(json);
      this.ensureDir(this.recvStagingRoot());
      this.destRoot = this.recvStagingRoot();
      this.destReady = true;
      if (this.recvDestUri.length === 0) {
        void this.ensureRecvDest(false).then((ok: boolean) => {
          if (ok) {
            this.banner = `已上线 · 保存到 ${this.destLabel}`;
          }
        });
      }
      if (!this.paired) {
        await this.runBeginPairWait();
      } else {
        this.banner = this.liveBanner;
        void this.startServeLoop();
      }
    } catch (e) {
      const first = this.humanizeNativeError(e);
      // Rare on phone: previous Ability still holding port — drop and retry once.
      if (String(e).indexOf('port_in_use') >= 0 || first.indexOf('端口') >= 0) {
        try {
          sessionGoOffline();
          const port = this.listenPortEdit > 0 ? this.listenPortEdit : 0;
          const json = await sessionGoOnline(port);
          this.applyStatus(json);
          this.error = '';
          this.ensureDir(this.recvStagingRoot());
          this.destRoot = this.recvStagingRoot();
          this.destReady = true;
          if (!this.paired) {
            await this.runBeginPairWait();
          } else {
            this.banner = this.liveBanner;
            void this.startServeLoop();
          }
          return;
        } catch (e2) {
          this.error = this.humanizeNativeError(e2) || first;
          this.banner = '上线失败';
          return;
        }
      }
      this.error = first;
      this.banner = '上线失败';
    } finally {
      this.busy = false;
    }
  }

  /** Arm background pair wait (non-blocking); code comes from status. */
  async runBeginPairWait(): Promise<void> {
    if (this.paired || this.connectionLabel === '等待配对') {
      return;
    }
    try {
      await sessionBeginPair();
      const json = await sessionStatus();
      this.applyStatus(json);
      this.banner = this.liveBanner;
    } catch (e) {
      this.error = this.humanizeNativeError(e);
    }
  }

  async onPairNearby(p: NearbyPeerObj): Promise<void> {
    if (p.trusted || this.busy) {
      return;
    }
    const code = this.pairCodeEdit.trim();
    if (code.length < 6) {
      this.error = '请先输入对方屏幕上的 6 位配对码';
      return;
    }
    this.busy = true;
    this.error = '';
    try {
      const json = await sessionPair(p.host, p.port, code);
      this.applyStatus(json);
      this.pairCodeEdit = '';
      this.banner = this.liveBanner;
      void this.notifyAlert(Index.NOTIF_ID_PAIR, 'LanBridge', '已配对');
      void this.startServeLoop();
    } catch (e) {
      this.error = this.humanizeNativeError(e);
    } finally {
      this.busy = false;
    }
  }

  /** Confirm code + pair nearby, or fall back to manual IP when nearby is empty. */
  async onConfirmPair(): Promise<void> {
    if (this.busy || this.paired) {
      return;
    }
    const code = this.pairCodeEdit.trim();
    if (code.length < 6) {
      this.error = '请先填满对方 6 位配对码';
      return;
    }
    let target: NearbyPeerObj | undefined = undefined;
    for (let i = 0; i < this.nearbyList.length; i++) {
      if (!this.nearbyList[i].trusted) {
        target = this.nearbyList[i];
        break;
      }
    }
    if (target === undefined && this.nearbyList.length === 1) {
      target = this.nearbyList[0];
    }
    if (target === undefined) {
      this.manualPairUserClosed = false;
      this.showManualPair = true;
      if (this.peerHostEdit.trim().length > 0) {
        await this.onPair();
        return;
      }
      this.error = '附近暂无设备。请在下方填写对方本机 IP，再点「用 IP 配对」';
      return;
    }
    if (target.trusted) {
      await this.onProbePeer();
      return;
    }
    await this.onPairNearby(target);
  }

  async onRefreshNearby(): Promise<void> {
    this.error = '';
    try {
      const json = await sessionStatus();
      this.applyStatus(json);
      if (this.nearbyList.length === 0) {
        this.manualPairUserClosed = false;
        this.showManualPair = true;
        this.banner = '附近仍无设备（热点常拦广播）。看对方屏上的本机 IP，填到下方手填区';
      }
    } catch (e) {
      this.error = this.humanizeNativeError(e);
    }
  }

  toggleManualPair(): void {
    this.showManualPair = !this.showManualPair;
    this.manualPairUserClosed = !this.showManualPair;
  }

  async onBeginPair(): Promise<void> {
    if (!this.listening) {
      await this.onGoOnline();
      return;
    }
    this.busy = true;
    this.error = '';
    try {
      await this.runBeginPairWait();
    } finally {
      this.busy = false;
    }
  }

  onGoOffline(): void {
    this.serveAbort = true;
    this.serveLoop = false;
    this.pairingCode = '';
    try {
      const json = sessionGoOffline();
      if (json && json.length > 0) {
        this.applyStatus(json);
      }
      this.banner = this.stateLabel;
      this.error = '';
    } catch (e) {
      this.error = this.humanizeNativeError(e);
    }
  }

  /** Wipe trust so pair code + fill box come back (fixes one-sided reinstall). */
  async onClearPair(): Promise<void> {
    this.serveAbort = true;
    this.serveLoop = false;
    this.busy = true;
    this.error = '';
    try {
      // Bye first: without it the PC keeps「已连接」until its heartbeat window expires.
      const json = await sessionUnpair();
      if (json && json.length > 0) {
        this.applyStatus(json);
      }
      if (!this.listening) {
        await this.onGoOnline();
      } else {
        await this.runBeginPairWait();
      }
      this.clipHint = '已清除配对并通知对方。填对方 6 位码后点「确认配对」；附近为空时可手填对方 IP';
      this.banner = this.liveBanner;
    } catch (e) {
      this.error = this.humanizeNativeError(e);
    } finally {
      this.busy = false;
    }
  }

  /** User action: TCP probe so「对端离线」can become「已连接」without waiting for UDP. */
  async onProbePeer(): Promise<void> {
    if (!this.paired) {
      this.error = '尚未配对';
      return;
    }
    if (!this.listening) {
      await this.onGoOnline();
      if (!this.listening) {
        return;
      }
    }
    this.busy = true;
    this.error = '';
    this.banner = '正在重试连接…';
    try {
      let lastMsg = '';
      for (let attempt = 0; attempt < 8; attempt++) {
        try {
          const json = await sessionProbePeer();
          this.applyStatus(json);
          this.banner = this.liveBanner;
          this.error = '';
          this.clipHint = this.connectionLabel === '已连接' ? '对端可达' : '已探测，请再试收发';
          return;
        } catch (e) {
          lastMsg = String(e);
          if (lastMsg.indexOf('busy') >= 0 || lastMsg.indexOf('rc=45') >= 0) {
            await this.delayMs(350);
            continue;
          }
          break;
        }
      }
      this.error = '';
      this.banner = this.liveBanner;
      this.clipHint = this.humanizeNativeError(lastMsg);
      if (this.clipHint.length === 0) {
        this.clipHint = '对端未响应，确认对方已打开 LanBridge';
      }
    } finally {
      this.busy = false;
    }
  }

  async onPair(): Promise<void> {
    this.busy = true;
    this.error = '';
    this.banner = '连接中…';
    try {
      const json = await sessionPair(this.peerHostEdit.trim(), this.peerPortEdit || 37532, this.pairCodeEdit.trim());
      this.applyStatus(json);
      this.banner = '已配对';
      void this.notifyAlert(Index.NOTIF_ID_PAIR, 'LanBridge', '已配对');
      if (!this.listening) {
        await this.onGoOnline();
      } else {
        void this.startServeLoop();
      }
    } catch (e) {
      this.error = this.humanizeNativeError(e);
      this.banner = '配对失败';
    } finally {
      this.busy = false;
    }
  }

  onApplyPeer(): void {
    try {
      const json = sessionSetPeer(this.peerHostEdit.trim(), this.peerPortEdit || 37532);
      this.applyStatus(json);
      this.banner = '已更新对端地址';
      this.error = '';
    } catch (e) {
      this.error = this.humanizeNativeError(e);
    }
  }

  basenameFromUri(uri: string): string {
    const q = uri.split('?')[0];
    const parts = q.split('/');
    const last = parts.length > 0 ? parts[parts.length - 1] : '';
    return last.length > 0 ? decodeURIComponent(last) : 'file.bin';
  }

  copyUriToPath(srcUri: string, destPath: string): void {
    const src = fileIo.openSync(srcUri, fileIo.OpenMode.READ_ONLY);
    try {
      const st = fileIo.statSync(src.fd);
      const parent = destPath.substring(0, destPath.lastIndexOf('/'));
      this.ensureDir(parent);
      const dst = fileIo.openSync(destPath, fileIo.OpenMode.READ_WRITE | fileIo.OpenMode.CREATE | fileIo.OpenMode.TRUNC);
      try {
        this.copyFdFully(src.fd, dst.fd, st.size);
      } finally {
        fileIo.closeSync(dst);
      }
    } finally {
      fileIo.closeSync(src);
    }
  }

  copyPathToUri(srcPath: string, destUri: string): void {
    const src = fileIo.openSync(srcPath, fileIo.OpenMode.READ_ONLY);
    try {
      const st = fileIo.statSync(src.fd);
      const dst = fileIo.openSync(
        destUri,
        fileIo.OpenMode.READ_WRITE | fileIo.OpenMode.CREATE | fileIo.OpenMode.TRUNC
      );
      try {
        this.copyFdFully(src.fd, dst.fd, st.size);
      } finally {
        fileIo.closeSync(dst);
      }
    } finally {
      fileIo.closeSync(src);
    }
  }

  /** Chunked copy; verifies read/write byte counts (vendor readSync/writeSync return lengths). */
  copyFdFully(srcFd: number, dstFd: number, total: number): void {
    const chunk = 1024 * 1024;
    let copied = 0;
    while (copied < total) {
      const want = Math.min(chunk, total - copied);
      const buf = new ArrayBuffer(want);
      const nRead = fileIo.readSync(srcFd, buf);
      if (nRead !== want) {
        throw new Error(`读文件不完整：${nRead}/${want} @${copied}`);
      }
      const nWrite = fileIo.writeSync(dstFd, buf);
      if (nWrite !== want) {
        throw new Error(`写文件不完整：${nWrite}/${want} @${copied}`);
      }
      copied += want;
    }
  }

  async pickAnyFileUris(): Promise<Array<string>> {
    const opts = new picker.DocumentSelectOptions();
    // Vendor: DocumentSelectOptions.maxSelectNumber 1–500, default 500.
    opts.maxSelectNumber = 50;
    const documentPicker = new picker.DocumentViewPicker(this.hostCtx());
    const uris: Array<string> = await documentPicker.select(opts);
    if (!uris || uris.length === 0) {
      throw new Error('未选择文件');
    }
    return uris;
  }

  async exportFileToUser(localPath: string, suggestedName: string): Promise<string> {
    const saveOpts = new picker.DocumentSaveOptions();
    saveOpts.newFileNames = [suggestedName];
    const documentPicker = new picker.DocumentViewPicker(this.hostCtx());
    const uris: Array<string> = await documentPicker.save(saveOpts);
    if (!uris || uris.length === 0) {
      throw new Error('未选择保存位置');
    }
    this.copyPathToUri(localPath, uris[0]);
    return uris[0];
  }

  async onPickSendFile(): Promise<void> {
    try {
      const uris = await this.pickAnyFileUris();
      this.ensureDir(this.stageRoot());
      const next: Array<SendQueuedFile> = this.sendQueue.slice();
      for (let i = 0; i < uris.length; i++) {
        const uri = uris[i];
        const name = this.basenameFromUri(uri);
        const staged = `${this.stageRoot()}/${Date.now()}_${i}_${name}`;
        this.copyUriToPath(uri, staged);
        next.push({ path: staged, name: name });
      }
      this.sendQueue = next;
      this.banner = `已选 ${uris.length} 个文件，队列 ${this.sendQueue.length}`;
      this.error = '';
    } catch (e) {
      this.error = String(e);
    }
  }

  clearSendQueue(): void {
    this.sendQueue = [];
  }

  /** Write last outbound probe/transfer metrics for LAN 85% bench (hdc pull). */
  persistLastSendBench(): void {
    try {
      const json = sessionStatus();
      const s = this.parseStatus(json);
      const probe = s.last_send_probe_bps !== undefined ? s.last_send_probe_bps : 0;
      const transfer = s.last_send_transfer_bps !== undefined ? s.last_send_transfer_bps : 0;
      const bytes = s.last_send_bytes !== undefined ? s.last_send_bytes : 0;
      if (probe <= 0 || transfer <= 0 || bytes <= 0) {
        return;
      }
      const ratio = Math.round((transfer * 1000) / probe);
      const body =
        `# LanBridge last send bench\n\n` +
        `- scope: phone guest → PC host (shell send)\n` +
        `- probe_bps: ${probe}\n` +
        `- probe_elapsed_us: ${s.last_send_probe_elapsed_us !== undefined ? s.last_send_probe_elapsed_us : 0}\n` +
        `- transfer_bps: ${transfer}\n` +
        `- transfer_elapsed_us: ${s.last_send_transfer_elapsed_us !== undefined ? s.last_send_transfer_elapsed_us : 0}\n` +
        `- transfer_bytes: ${bytes}\n` +
        `- transfer_per_mille_of_probe: ${ratio}\n` +
        `- pass_85pct: ${ratio >= 850 ? 'YES' : 'NO'}\n`;
      const dir = `${this.hostCtx().filesDir}/lb-session`;
      this.ensureDir(dir);
      const path = `${dir}/last-send-bench.md`;
      const f = fileIo.openSync(path, fileIo.OpenMode.READ_WRITE | fileIo.OpenMode.CREATE | fileIo.OpenMode.TRUNC);
      try {
        fileIo.writeSync(f.fd, body);
      } finally {
        fileIo.closeSync(f);
      }
    } catch (_e) {
      // best-effort for bench scripts
    }
  }

  /**
   * Consume Share Kit payload from EntryAbility (AppStorage).
   * API: @ohos.app.ability.want / Share Kit (`want.parameters` + UDMF uris).
   */
  async consumeSharePending(): Promise<void> {
    const raw = AppStorage.get<string>('lbSharePendingJson');
    if (raw === undefined || raw === null || raw.length === 0) {
      return;
    }
    // Clear first so PageShow + Watch do not double-ingest.
    AppStorage.set('lbSharePendingJson', '');
    let payload: LbSharePendingPayload;
    try {
      payload = JSON.parse(raw) as LbSharePendingPayload;
    } catch (_e) {
      this.error = '分享数据解析失败';
      return;
    }
    const texts = payload.texts !== undefined && payload.texts !== null ? payload.texts : [];
    const uris = payload.uris !== undefined && payload.uris !== null ? payload.uris : [];
    if (texts.length === 0 && uris.length === 0) {
      return;
    }

    // Cap at skills maxFileSupported (50).
    const uriCap = uris.length > 50 ? uris.slice(0, 50) : uris;
    try {
      if (uriCap.length > 0) {
        this.ensureDir(this.stageRoot());
        const next: Array<SendQueuedFile> = this.sendQueue.slice();
        for (let i = 0; i < uriCap.length; i++) {
          const uri = uriCap[i];
          const name = this.basenameFromUri(uri);
          const staged = `${this.stageRoot()}/share_${Date.now()}_${i}_${name}`;
          this.copyUriToPath(uri, staged);
          next.push({ path: staged, name: name });
        }
        this.sendQueue = next;
      }
      for (let t = 0; t < texts.length; t++) {
        const tx = texts[t].trim();
        if (tx.length > 0) {
          this.shareTextsPending.push(tx);
        }
      }
      if (texts.length > 0 && this.shareTextsPending.length > 0) {
        this.clipText = this.shareTextsPending[this.shareTextsPending.length - 1];
      }
      const fileN = uriCap.length;
      const textN = texts.length;
      this.banner = `已接收分享 · 文件 ${fileN} · 文本 ${textN}`;
      this.error = '';
      if (!this.canOperate) {
        this.clipHint = '分享已暂存：请先配对并上线，配对后将自动发送到电脑';
        if (!this.paired) {
          this.showManualPair = true;
        }
        return;
      }
      await this.tryFlushPendingShare();
    } catch (e) {
      this.error = `分享入库失败：${String(e)}`;
    }
  }

  /** Send staged share files + clipboard texts once paired/listening and idle. */
  async tryFlushPendingShare(): Promise<void> {
    if (this.shareFlushBusy || this.busy || !this.canOperate) {
      return;
    }
    const hasFiles = this.sendQueue.length > 0;
    const hasTexts = this.shareTextsPending.length > 0;
    if (!hasFiles && !hasTexts) {
      return;
    }
    this.shareFlushBusy = true;
    try {
      if (hasFiles) {
        await this.onSendFile();
      }
      while (this.shareTextsPending.length > 0) {
        const tx = this.shareTextsPending.shift() as string;
        this.clipText = tx;
        await this.onSendClipText();
      }
      this.clipHint = '分享内容已发送到电脑';
    } catch (e) {
      this.error = this.humanizeNativeError(e);
    } finally {
      this.shareFlushBusy = false;
    }
  }

  delayMs(ms: number): Promise<void> {
    return new Promise<void>((resolve) => {
      setTimeout(() => {
        resolve();
      }, ms);
    });
  }

  async sendOneWithBusyRetry(path: string, name: string): Promise<number> {
    let lastMsg = 'send failed';
    for (let attempt = 0; attempt < 16; attempt++) {
      try {
        return await sessionSendFile(path, name);
      } catch (e) {
        lastMsg = String(e);
        const soft = lastMsg.indexOf('busy') >= 0 || lastMsg.indexOf('rc=45') >= 0 ||
          lastMsg.indexOf('peer_unreachable') >= 0;
        if (soft && attempt < 15) {
          await this.delayMs(400 + attempt * 100);
          continue;
        }
        throw new Error(lastMsg);
      }
    }
    throw new Error(lastMsg);
  }

  async onSendFile(): Promise<void> {
    if (this.sendQueue.length === 0) {
      this.error = '请先选择文件';
      return;
    }
    this.busy = true;
    this.error = '';
    const batch = this.sendQueue.slice();
    this.sendQueue = [];
    const jobIds: Array<string> = [];
    for (let i = 0; i < batch.length; i++) {
      const id = `send-${Date.now()}-${i}-${batch[i].name}`;
      jobIds.push(id);
      this.pushJob({
        id: id,
        kind: 'send',
        title: batch[i].name,
        detail: `排队 ${i + 1}/${batch.length}`,
        status: 'queued',
        path: batch[i].path,
        rev: 1
      });
    }
    let ok = 0;
    try {
      for (let i = 0; i < batch.length; i++) {
        const f = batch[i];
        const id = jobIds[i];
        this.activeJobId = id;
        // Initial line; poll will replace with % · rate while NAPI send runs.
        this.updateJob(id, 'active', `发送中 ${i + 1}/${batch.length} · 等待进度…`, f.path);
        this.banner = `发送中 ${i + 1}/${batch.length} · ${f.name}`;
        const n = await this.sendOneWithBusyRetry(f.path, f.name);
        ok++;
        // Keep activeJobId on the done card so status poll does not spawn a
        // second 「发送中」while NAPI finishes short teardown.
        this.updateJob(id, 'done', `已发送 · ${this.formatBytes(n)}`, f.path);
        this.persistLastSendBench();
      }
      this.banner = `发送完成 · ${ok}/${batch.length} 个文件`;
      this.error = '';
      await this.refreshStatusKeepBanner();
      this.persistLastSendBench();
      this.activeJobId = '';
    } catch (e) {
      const msg = this.humanizeNativeError(e);
      // Prefer job-card failure; avoid sticky red that contradicts「已连接」banner.
      const jobMsg = msg.length > 0 ? msg : '发送失败';
      this.banner = `发送失败（已成功 ${ok}/${batch.length}）`;
      if (ok < jobIds.length) {
        this.updateJob(jobIds[ok], 'fail', jobMsg);
      }
      this.activeJobId = '';
      if (this.linkOk()) {
        this.error = '';
        this.clipHint = jobMsg;
      } else {
        this.error = jobMsg;
      }
      const rest: Array<SendQueuedFile> = [];
      for (let j = ok; j < batch.length; j++) {
        rest.push(batch[j]);
      }
      this.sendQueue = rest;
    } finally {
      this.busy = false;
    }
  }

  writeOsClipboard(text: string): boolean {
    try {
      const data = pasteboard.createData(pasteboard.MIMETYPE_TEXT_PLAIN, text);
      pasteboard.getSystemPasteboard().setDataSync(data);
      this.lastClipText = text;
      this.clipSelfWriteUntil = Date.now() + 3000;
      return true;
    } catch (e) {
      this.error = `写剪贴板失败：${String(e)}`;
      return false;
    }
  }

  async writeOsClipboardPng(png: ArrayBuffer): Promise<boolean> {
    try {
      const src: image.ImageSource = image.createImageSource(png);
      const pixelMap: image.PixelMap = await src.createPixelMap();
      const data = pasteboard.createData(pasteboard.MIMETYPE_PIXELMAP, pixelMap);
      pasteboard.getSystemPasteboard().setDataSync(data);
      this.clipSelfWriteUntil = Date.now() + 3000;
      // Re-reading the pasteboard re-encodes the image, so bytes will not compare equal to the
      // inbound PNG: record whatever we read back as the baseline instead of echoing it out.
      this.clipInboundPngPending = true;
      return true;
    } catch (e) {
      this.error = `写图片剪贴板失败：${String(e)}`;
      return false;
    }
  }

  /**
   * Image half of the clipboard. HarmonyOS keeps pixels as PixelMap, so read it and pack to PNG
   * (the wire format both sides share). Returns null when there is no image.
   */
  async readOsClipboardPng(): Promise<ArrayBuffer | null> {
    try {
      const data = pasteboard.getSystemPasteboard().getDataSync();
      const pixelMap: image.PixelMap | undefined = data.getPrimaryPixelMap();
      if (pixelMap === undefined || pixelMap === null) {
        return null;
      }
      const packer: image.ImagePacker = image.createImagePacker();
      const option: image.PackingOption = { format: 'image/png', quality: 100 };
      const buf: ArrayBuffer = await packer.packing(pixelMap, option);
      await packer.release();
      return buf;
    } catch (_e) {
      return null;
    }
  }

  /** Cheap content signature so the same image is not pushed twice. */
  private pngSig(buf: ArrayBuffer): string {
    const view = new Uint8Array(buf);
    let h = 0;
    const n = view.length < 512 ? view.length : 512;
    for (let i = 0; i < n; i++) {
      h = (h * 31 + view[i]) % 2147483647;
    }
    return view.length.toString() + ':' + h.toString();
  }

  readOsClipboardText(): string {
    try {
      const board = pasteboard.getSystemPasteboard();
      const data = board.getDataSync();
      const text = data.getPrimaryText();
      return text !== undefined && text !== null ? text : '';
    } catch (_e) {
      return '';
    }
  }

  /** True auto-sync: READ_PASTEBOARD + pasteboard.on('update'), same UX as Win. */
  async isPasteboardGranted(): Promise<boolean> {
    try {
      const at = abilityAccessCtrl.createAtManager();
      const tokenId = this.hostCtx().applicationInfo.accessTokenId;
      const st = await at.checkAccessToken(tokenId, 'ohos.permission.READ_PASTEBOARD');
      // 0 == PERMISSION_GRANTED
      return st === 0;
    } catch (_e) {
      return false;
    }
  }

  async requestPasteboardPermission(): Promise<boolean> {
    try {
      const at = abilityAccessCtrl.createAtManager();
      const perms: Array<Permissions> = ['ohos.permission.READ_PASTEBOARD'];
      const data = await at.requestPermissionsFromUser(this.hostCtx(), perms);
      if (data.authResults.length > 0 && data.authResults[0] === 0) {
        return true;
      }
      return false;
    } catch (_e) {
      return false;
    }
  }

  /**
   * Keep READ_PASTEBOARD + pasteboard watch aligned with system grant.
   * - Granted → start watch
   * - Lost / never granted → auto system dialog (with cooldown after deny)
   * Never rely on a manual「复查」button.
   */
  async reconcileClipPermission(allowRequest: boolean): Promise<void> {
    if (this.clipPermAsking || !this.canOperate) {
      return;
    }
    const granted = await this.isPasteboardGranted();
    this.clipLastCheckMs = Date.now();
    if (granted) {
      this.clipHadGrant = true;
      this.clipAutoOk = true;
      this.startClipWatch();
      if (this.clipHint.indexOf('失败') < 0 && this.clipHint.indexOf('未授权') < 0) {
        this.clipHint = '复制即同步到电脑';
      }
      return;
    }
    const lost = this.clipHadGrant || this.clipAutoOk;
    this.clipAutoOk = false;
    this.clipHadGrant = false;
    this.stopClipWatch();
    if (!allowRequest) {
      this.clipHint = '读剪贴板未授权，自动同步暂停';
      return;
    }
    const now = Date.now();
    if (now < this.clipPermCooldownUntil) {
      this.clipHint = '读剪贴板未授权；稍后会自动再申请，或用下方「粘贴」后备';
      return;
    }
    // First need, or grant expired/revoked → popup automatically.
    this.clipPermAsking = true;
    try {
      const ok = await this.requestPasteboardPermission();
      this.clipAutoOk = ok;
      if (ok) {
        this.clipHadGrant = true;
        this.startClipWatch();
        this.clipHint = '复制即同步到电脑（已授权）';
      } else {
        // User denied: cool down so status poll does not spam the dialog.
        this.clipPermCooldownUntil = now + 90000;
        this.clipHint = lost ?
          '读剪贴板权限已失效且未再授权；90 秒后自动再试，或用「粘贴」后备' :
          '未授权读剪贴板；90 秒后自动再试，或用「粘贴」后备';
      }
    } finally {
      this.clipPermAsking = false;
    }
  }

  /** Status-poll entry: re-check at most every 5s; auto-request on expiry. */
  async ensureClipAutoSilent(): Promise<void> {
    if (!this.canOperate || this.clipPermAsking) {
      return;
    }
    const now = Date.now();
    if (this.clipAutoOk && this.clipWatching && (now - this.clipLastCheckMs) < 5000) {
      return;
    }
    await this.reconcileClipPermission(true);
  }

  /** 相册读权限：READ_IMAGEVIDEO 需要在运行时申请一次（module.json5 声明 + ACL 已放行）。 */
  async ensureImageVideoPermission(): Promise<boolean> {
    try {
      const at = abilityAccessCtrl.createAtManager();
      const tokenId = this.hostCtx().applicationInfo.accessTokenId;
      const st = await at.checkAccessToken(tokenId, 'ohos.permission.READ_IMAGEVIDEO');
      if (st === 0) {
        return true;
      }
      const perms: Array<Permissions> = ['ohos.permission.READ_IMAGEVIDEO'];
      const res = await at.requestPermissionsFromUser(this.hostCtx(), perms);
      return res.authResults.length > 0 && res.authResults[0] === 0;
    } catch (_e) {
      return false;
    }
  }

  async toggleShotAuto(): Promise<void> {
    if (this.shotAuto) {
      this.stopShotWatch();
      this.shotAuto = false;
      this.shotHint = '已关闭截图自动共享';
      return;
    }
    const ok = await this.ensureImageVideoPermission();
    if (!ok) {
      this.shotHint = '未授权读取相册，无法自动共享截图';
      return;
    }
    this.shotAuto = true;
    this.shotHint = '截图后自动发到电脑（缩略图）';
    this.startShotWatch();
  }

  startShotWatch(): void {
    if (this.shotWatching) {
      return;
    }
    this.shotWatching = true;
    // 记住当前最新一张作为基线，避免把历史截图全发出去。
    void this.pollShotOnce(true);
    this.shotTimer = setInterval((): void => {
      void this.pollShotOnce(false);
    }, 2500);
  }

  stopShotWatch(): void {
    if (this.shotTimer >= 0) {
      clearInterval(this.shotTimer);
      this.shotTimer = -1;
    }
    this.shotWatching = false;
  }

  /** 轮询媒体库最新一张（date_added 倒序），命中新截图则转 PNG 发送。 */
  async pollShotOnce(baseline: boolean): Promise<void> {
    if (!this.canOperate || this.shotSending) {
      return;
    }
    try {
      const helper = photoAccessHelper.getPhotoAccessHelper(this.hostCtx());
      const pred = new dataSharePredicates.DataSharePredicates();
      pred.orderByDesc('date_added');
      const opts: photoAccessHelper.FetchOptions = {
        fetchColumns: [
          photoAccessHelper.PhotoKeys.DISPLAY_NAME,
          photoAccessHelper.PhotoKeys.DATE_ADDED,
          photoAccessHelper.PhotoKeys.WIDTH,
          photoAccessHelper.PhotoKeys.HEIGHT
        ],
        predicates: pred
      };
      const res = await helper.getAssets(opts);
      const count = res.getCount();
      if (count === 0) {
        res.close();
        return;
      }
      const asset = await res.getFirstObject();
      const name = asset.displayName !== undefined && asset.displayName !== null ? asset.displayName : '';
      res.close();
      if (baseline) {
        this.lastShotName = name;
        this.shotHint = `相册已就绪 · 最新：${name.length > 0 ? name : '(空)'}`;
        return;
      }
      if (name.length === 0) {
        return;
      }
      if (name === this.lastShotName) {
        this.shotHint = `监听中 · 最新：${name}`;
        return;
      }
      this.lastShotName = name;
      console.info('[shot] new asset: ' + name);
      // 只发截图，避免把相机/下载的图片也发过去。
      if (!/screenshot|截图|截屏|snip/i.test(name)) {
        this.shotHint = `跳过非截图：${name}`;
        return;
      }
      this.shotSending = true;
      // 优先拿原图：申请该 URI 的临时读权限后直接读字节（API 14+）。失败再退回缩略图。
      let buf: ArrayBuffer | null = null;
      try {
        const granted: Array<string> = await helper.requestPhotoUrisReadPermission([asset.uri]);
        if (granted.length > 0) {
          const file = fileIo.openSync(granted[0], fileIo.OpenMode.READ_ONLY);
          const stat = fileIo.statSync(file.fd);
          const raw = new ArrayBuffer(stat.size);
          fileIo.readSync(file.fd, raw);
          fileIo.closeSync(file);
          const head = new Uint8Array(raw.slice(0, 4));
          const isPng = head[0] === 0x89 && head[1] === 0x50 && head[2] === 0x4E && head[3] === 0x47;
          if (isPng) {
            buf = raw;
          } else {
            const src: image.ImageSource = image.createImageSource(raw);
            const pm0: image.PixelMap = await src.createPixelMap();
            const packer0: image.ImagePacker = image.createImagePacker();
            buf = await packer0.packing(pm0, { format: 'image/png', quality: 100 });
            await packer0.release();
          }
        }
      } catch (_e) {
        buf = null;
      }
      if (buf === null) {
        // 退回缩略图：按原始宽高比取图，避免被裁边/压扁。
        let rawW = 0;
        let rawH = 0;
        try {
          rawW = Number(asset.get(photoAccessHelper.PhotoKeys.WIDTH));
          rawH = Number(asset.get(photoAccessHelper.PhotoKeys.HEIGHT));
        } catch (_e) {
          rawW = 0;
          rawH = 0;
        }
        const cap = 4096;
        let tw = rawW > 0 ? rawW : 1440;
        let th = rawH > 0 ? rawH : 1440;
        if (tw > cap || th > cap) {
          const k = Math.min(cap / tw, cap / th);
          tw = Math.max(1, Math.round(tw * k));
          th = Math.max(1, Math.round(th * k));
        }
        const pm: image.PixelMap = await asset.getThumbnail({ width: tw, height: th });
        const packer: image.ImagePacker = image.createImagePacker();
        buf = await packer.packing(pm, { format: 'image/png', quality: 100 });
        await packer.release();
      }
      console.info('[shot] sending ' + name + ' bytes=' + buf.byteLength);
      await this.sendClipPngWithRetry(buf);
      console.info('[shot] sent ' + name);
      this.shotHint = `已发送截图（${this.formatBytes(buf.byteLength)}）`;
      this.pushJob({
        id: `shot-${Date.now()}`,
        kind: 'clip',
        title: '自动发送截图',
        detail: name,
        status: 'done'
      });
    } catch (e) {
      // 权限/机型不支持时不要让功能静默消失：把原因显示出来，并打到 hilog 便于排查。
      const msg = String(e).slice(0, 120);
      console.error('[shot] fail: ' + msg);
      this.shotHint = '截图发送失败：' + msg;
    } finally {
      this.shotSending = false;
    }
  }

  /** 启动/回到前台时，如果开关是开的就自动恢复监听（权限已授权则无需再点）。 */
  async resumeShotWatchIfEnabled(): Promise<void> {
    if (!this.shotAuto || this.shotWatching) {
      return;
    }
    const ok = await this.ensureImageVideoPermission();
    if (ok) {
      this.startShotWatch();
    } else {
      this.shotHint = '相册权限未授予，截图不会自动发送';
    }
  }

  startClipWatch(): void {
    if (this.clipWatching) {
      return;
    }
    try {
      const board = pasteboard.getSystemPasteboard();
      const listener = (): void => {
        void this.onLocalClipChanged();
      };
      board.on('update', listener);
      this.clipPasteListener = listener;
      this.clipWatching = true;
    } catch (_e) {
      this.clipWatching = false;
      this.clipHint = '剪贴板监听注册失败';
    }
  }

  stopClipWatch(): void {
    if (!this.clipWatching) {
      return;
    }
    try {
      const board = pasteboard.getSystemPasteboard();
      if (this.clipPasteListener !== null) {
        board.off('update', this.clipPasteListener);
      } else {
        board.off('update');
      }
    } catch (_e) {
      // ignore
    }
    this.clipPasteListener = null;
    this.clipWatching = false;
  }

  private isAppForeground(): boolean {
    const v = AppStorage.get<boolean>('lbForeground');
    return v === undefined ? true : v;
  }

  private clearBgTaskRefreshTimer(): void {
    if (this.bgTaskRefreshTimer >= 0) {
      clearTimeout(this.bgTaskRefreshTimer);
      this.bgTaskRefreshTimer = -1;
    }
  }

  private scheduleBgTaskRefresh(): void {
    this.clearBgTaskRefreshTimer();
    this.bgTaskRefreshTimer = setTimeout((): void => {
      this.bgTaskRefreshTimer = -1;
      if (!this.canOperate) {
        return;
      }
      // Re-assert continuous task before DATA_TRANSFER idle cancel (~10 min).
      void this.restartContinuousTask();
    }, Index.BG_TASK_REFRESH_MS);
  }

  async ensureContinuousTask(): Promise<void> {
    if (!this.canOperate) {
      await this.stopContinuousTask();
      return;
    }
    if (this.bgTaskOk || this.bgTaskStarting) {
      return;
    }
    await this.startContinuousTask();
  }

  private async restartContinuousTask(): Promise<void> {
    await this.stopContinuousTask();
    if (this.canOperate) {
      await this.startContinuousTask();
    }
  }

  private async startContinuousTask(): Promise<void> {
    if (this.bgTaskStarting || this.bgTaskOk || !this.canOperate) {
      return;
    }
    this.bgTaskStarting = true;
    try {
      const ctx = this.hostCtx();
      const wantAgentInfo: wantAgent.WantAgentInfo = {
        wants: [
          {
            bundleName: 'com.lanbridge.ohosshell',
            abilityName: 'EntryAbility'
          }
        ],
        actionType: wantAgent.OperationType.START_ABILITY,
        requestCode: 0,
        wantAgentFlags: [wantAgent.WantAgentFlags.UPDATE_PRESENT_FLAG]
      };
      const agent: WantAgent = await wantAgent.getWantAgent(wantAgentInfo);
      // dataTransfer: file/clip payload; multiDeviceConnection: peer listen / LAN link.
      const modes: string[] = ['dataTransfer', 'multiDeviceConnection'];
      const res: backgroundTaskManager.ContinuousTaskNotification =
        await backgroundTaskManager.startBackgroundRunning(ctx, modes, agent);
      this.bgTaskNotifId = res.notificationId;
      this.lastLivePct = -1;
      this.bgTaskOk = true;
      this.scheduleBgTaskRefresh();
      void this.ensureNotificationEnabled();
      if (this.clipHint.indexOf('失败') < 0) {
        this.clipHint = this.clipAutoOk ?
          '后台可收 PC 剪贴板；本机复制请切回本应用后同步' :
          '后台接收已开启；请授权读剪贴板以推送到电脑';
      }
    } catch (e) {
      this.bgTaskOk = false;
      this.bgTaskNotifId = -1;
      const err = e as BusinessError;
      const msg = err !== undefined && err.message !== undefined ? err.message : String(e);
      this.clipHint = `后台保活未开启（退后台可能收不到 PC 剪贴板）：${msg}`;
    } finally {
      this.bgTaskStarting = false;
    }
  }

  private async stopContinuousTask(): Promise<void> {
    this.clearBgTaskRefreshTimer();
    if (!this.bgTaskOk && !this.bgTaskStarting) {
      // Still try stop in case native thinks it is running.
    }
    try {
      await backgroundTaskManager.stopBackgroundRunning(this.hostCtx());
    } catch (_e) {
      // ignore — may not have been started
    }
    this.bgTaskOk = false;
    this.bgTaskNotifId = -1;
    this.lastLivePct = -1;
  }

  /**
   * HarmonyOS blocks third-party apps from reading pasteboard while backgrounded.
   * update may fire in bg with empty getData — mark dirty and flush on foreground.
   */
  async flushClipAfterForeground(): Promise<void> {
    if (!this.canOperate || this.clipAutoBusy) {
      return;
    }
    await this.reconcileClipPermission(true);
    if (!this.clipAutoOk) {
      return;
    }
    const dirty = AppStorage.get<boolean>('lbClipDirty') === true;
    AppStorage.setOrCreate('lbClipDirty', false);
    const text = this.readOsClipboardText().trim();
    if (text.length === 0) {
      const png = await this.readOsClipboardPng();
      if (png !== null) {
        if (this.clipInboundPngPending) {
          this.lastClipPngSig = this.pngSig(png);
          this.clipInboundPngPending = false;
          return;
        }
        const sig = this.pngSig(png);
        if (sig !== this.lastClipPngSig) {
          this.clipAutoBusy = true;
          try {
            await this.sendClipPngWithRetry(png);
            this.lastClipPngSig = sig;
            this.clipHint = `已同步图片到电脑（${this.formatBytes(png.byteLength)}）`;
            void this.notifyAlert(Index.NOTIF_ID_CLIP, 'LanBridge', '已同步图片到电脑');
          } catch (e) {
            const msg = this.humanizeNativeError(e);
            this.clipHint = msg.length > 0 ? `图片同步失败：${msg}` : '图片同步失败';
          } finally {
            this.clipAutoBusy = false;
          }
        }
        return;
      }
      if (dirty) {
        this.clipHint = '已回到前台，但读不到剪贴板内容（可能已被清空）';
      }
      return;
    }
    if (!dirty && text === this.lastClipText) {
      return;
    }
    this.clipAutoBusy = true;
    try {
      await this.sendClipTextWithRetry(text);
      this.lastClipText = text;
      this.clipHint = `已同步到电脑（${this.formatBytes(text.length)}）`;
      void this.notifyAlert(Index.NOTIF_ID_CLIP, 'LanBridge', '已同步剪贴板到电脑');
      this.pushJob({
        id: `clip-auto-${Date.now()}`,
        kind: 'clip',
        title: '自动同步',
        detail: text.substring(0, 48),
        status: 'done'
      });
    } catch (e) {
      const msg = this.humanizeNativeError(e);
      this.clipHint = msg.length > 0 ? `同步失败：${msg}` : '同步失败';
    } finally {
      this.clipAutoBusy = false;
    }
  }

  async onLocalClipChanged(): Promise<void> {
    if (!this.canOperate || this.clipAutoBusy) {
      return;
    }
    // We wrote this pasteboard entry ourselves (inbound sync) — do not send it back.
    if (Date.now() < this.clipSelfWriteUntil) {
      return;
    }
    // Background: OS forbids getData — only remember dirty for foreground flush.
    if (!this.isAppForeground()) {
      AppStorage.setOrCreate('lbClipDirty', true);
      this.clipHint = '已检测到复制；切回 LanBridge 后立即同步（鸿蒙禁止后台读剪贴板）';
      return;
    }
    await this.reconcileClipPermission(true);
    if (!this.clipAutoOk) {
      return;
    }
    const text = this.readOsClipboardText().trim();
    if (text.length === 0 || text === this.lastClipText) {
      // No new text — it may be a screenshot (pixels only).
      const png = await this.readOsClipboardPng();
      if (png === null) {
        return;
      }
      if (this.clipInboundPngPending) {
        // This image came from the PC; just adopt it as the baseline.
        this.lastClipPngSig = this.pngSig(png);
        this.clipInboundPngPending = false;
        return;
      }
      const sig = this.pngSig(png);
      if (sig === this.lastClipPngSig) {
        return;
      }
      this.clipAutoBusy = true;
      try {
        await this.sendClipPngWithRetry(png);
        this.lastClipPngSig = sig;
        this.clipHint = `已自动同步图片（${this.formatBytes(png.byteLength)}）`;
        this.pushJob({
          id: `clip-auto-img-${Date.now()}`,
          kind: 'clip',
          title: '自动同步（图片）',
          detail: `${this.formatBytes(png.byteLength)} PNG`,
          status: 'done'
        });
      } catch (e) {
        const msg = this.humanizeNativeError(e);
        this.clipHint = msg.length > 0 ? `图片同步失败：${msg}` : '图片同步失败';
      } finally {
        this.clipAutoBusy = false;
      }
      return;
    }
    this.clipAutoBusy = true;
    try {
      await this.sendClipTextWithRetry(text);
      this.lastClipText = text;
      this.clipHint = `已自动同步（${this.formatBytes(text.length)}）`;
      this.pushJob({
        id: `clip-auto-${Date.now()}`,
        kind: 'clip',
        title: '自动同步',
        detail: text.substring(0, 48),
        status: 'done'
      });
    } catch (e) {
      const msg = this.humanizeNativeError(e);
      this.clipHint = msg.length > 0 ? `自动同步失败：${msg}` : '自动同步失败';
      this.clipAutoOk = false;
      this.clipHadGrant = false;
      this.clipPermCooldownUntil = 0;
      this.stopClipWatch();
      void this.reconcileClipPermission(true);
    } finally {
      this.clipAutoBusy = false;
    }
  }

  /** Fallback only when READ_PASTEBOARD is denied. */
  async onPasteButtonClick(_event: ClickEvent, result: PasteButtonOnClickResult): Promise<void> {
    if (result !== PasteButtonOnClickResult.SUCCESS) {
      this.clipHint = '未获得粘贴授权';
      return;
    }
    const text = this.readOsClipboardText().trim();
    if (text.length === 0) {
      this.clipHint = '剪贴板没有文本';
      return;
    }
    this.clipText = text;
    if (!this.canOperate) {
      this.clipHint = '未上线/未配对，文本已填入输入框';
      return;
    }
    await this.onSendClipText();
  }

  async sendClipTextWithRetry(text: string): Promise<void> {
    let lastMsg = 'send clip failed';
    for (let attempt = 0; attempt < 16; attempt++) {
      try {
        await sessionSendClipboardText(text);
        return;
      } catch (e) {
        lastMsg = String(e);
        const soft = lastMsg.indexOf('busy') >= 0 || lastMsg.indexOf('rc=45') >= 0 ||
          lastMsg.indexOf('peer_unreachable') >= 0;
        if (soft && attempt < 15) {
          await this.delayMs(400 + attempt * 100);
          continue;
        }
        throw new Error(lastMsg);
      }
    }
    throw new Error(lastMsg);
  }

  async sendClipPngWithRetry(png: ArrayBuffer): Promise<void> {
    let lastMsg = 'send clip png failed';
    for (let attempt = 0; attempt < 16; attempt++) {
      try {
        await sessionSendClipboardPng(png);
        return;
      } catch (e) {
        lastMsg = String(e);
        const soft = lastMsg.indexOf('busy') >= 0 || lastMsg.indexOf('rc=45') >= 0 ||
          lastMsg.indexOf('peer_unreachable') >= 0;
        if (soft && attempt < 15) {
          await this.delayMs(400 + attempt * 100);
          continue;
        }
        throw new Error(lastMsg);
      }
    }
    throw new Error(lastMsg);
  }

  async onSendClipText(): Promise<void> {
    const text = this.clipText.trim();
    if (text.length === 0) {
      this.error = '没有可发送的文本（请先粘贴或输入）';
      return;
    }
    this.error = '';
    this.banner = '发送文本…';
    this.clipAutoBusy = true;
    try {
      await this.delayMs(80);
      await this.sendClipTextWithRetry(text);
      this.lastClipText = text;
      this.banner = '文本已发送';
      this.clipHint = `已推送（${this.formatBytes(text.length)}）`;
      this.pushJob({
        id: `clip-${Date.now()}`,
        kind: 'clip',
        title: '推送文本',
        detail: text.substring(0, 48),
        status: 'done'
      });
      this.error = '';
    } catch (e) {
      const msg = this.humanizeNativeError(e);
      const jobMsg = msg.length > 0 ? msg : '发送失败';
      this.banner = '发送失败';
      this.pushJob({
        id: `clip-${Date.now()}`,
        kind: 'clip',
        title: '推送失败',
        detail: jobMsg.substring(0, 120),
        status: 'fail'
      });
      if (this.linkOk()) {
        this.error = '';
        this.clipHint = jobMsg;
      } else {
        this.error = jobMsg;
      }
    } finally {
      this.clipAutoBusy = false;
    }
  }

  async pickPngUri(): Promise<string> {
    const opts = new picker.DocumentSelectOptions();
    opts.maxSelectNumber = 1;
    opts.fileSuffixFilters = ['.png'];
    const documentPicker = new picker.DocumentViewPicker(this.hostCtx());
    const uris: Array<string> = await documentPicker.select(opts);
    if (!uris || uris.length === 0) {
      throw new Error('未选择文件');
    }
    return uris[0];
  }

  readPngFromUri(uri: string): ArrayBuffer {
    const file = fileIo.openSync(uri, fileIo.OpenMode.READ_ONLY);
    try {
      const st = fileIo.statSync(file.fd);
      const buf = new ArrayBuffer(st.size);
      fileIo.readSync(file.fd, buf);
      return buf;
    } finally {
      fileIo.closeSync(file);
    }
  }

  async onPickPng(): Promise<void> {
    try {
      const uri = await this.pickPngUri();
      this.clipPng = this.readPngFromUri(uri);
      this.clipImageLabel = this.basenameFromUri(uri);
      this.banner = `PNG：${this.clipImageLabel}`;
    } catch (e) {
      this.error = String(e);
    }
  }

  async onSendClipPng(): Promise<void> {
    if (this.clipPng === null) {
      this.error = '请先选择 PNG';
      return;
    }
    this.busy = true;
    this.banner = '发送图片…';
    try {
      await sessionSendClipboardPng(this.clipPng);
      this.banner = '图片已发送';
      this.error = '';
    } catch (e) {
      this.error = this.humanizeNativeError(e);
      this.banner = '发送失败';
    } finally {
      this.busy = false;
    }
  }

  base64ToArrayBuffer(b64: string): ArrayBuffer {
    // ArkTS: use buffer from native event; png_base64 decoded via Uint8Array manual
    const table = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
    const clean = b64.replace(/=+$/, '');
    const out: number[] = [];
    let i = 0;
    while (i + 3 < clean.length + 4 && i < clean.length) {
      const a = table.indexOf(clean.charAt(i));
      const b = table.indexOf(clean.charAt(i + 1));
      const c = i + 2 < clean.length ? table.indexOf(clean.charAt(i + 2)) : -1;
      const d = i + 3 < clean.length ? table.indexOf(clean.charAt(i + 3)) : -1;
      i += 4;
      out.push((a << 2) | (b >> 4));
      if (c >= 0) {
        out.push(((b & 15) << 4) | (c >> 2));
      }
      if (d >= 0) {
        out.push(((c & 3) << 6) | d);
      }
    }
    const ab = new ArrayBuffer(out.length);
    const u8 = new Uint8Array(ab);
    for (let j = 0; j < out.length; j++) {
      u8[j] = out[j];
    }
    return ab;
  }

  /** Concurrent accepts so clipboard can land while a file recv is in progress. */
  async startServeLoop(): Promise<void> {
    if (!this.destReady) {
      this.banner = `${this.stateLabel} · 自动接收目录已就绪`;
      this.ensureDir(this.recvStagingRoot());
      this.destRoot = this.recvStagingRoot();
      this.destReady = true;
    }
    this.serveAbort = false;
    while (this.serveWorkers < Index.SERVE_WORKERS) {
      this.serveWorkers++;
      void this.serveWorker();
    }
  }

  async serveWorker(): Promise<void> {
    this.serveLoop = true;
    try {
      while (!this.serveAbort) {
        if (!this.listening || !this.paired) {
          break;
        }
        try {
          const json = await sessionServeOnce(this.destRoot);
          await this.handleServeEvent(JSON.parse(json) as ServeEventObj);
        } catch (e) {
          const s = String(e);
          if (this.serveAbort || s.indexOf('not_online') >= 0) {
            break;
          }
          if (s.indexOf('timeout') >= 0 || s.indexOf('timed out') >= 0 ||
            s.indexOf('busy') >= 0 || s.indexOf('waiting for trusted peer') >= 0) {
            continue;
          }
          if (this.isSoftPeerClose(s)) {
            this.error = '';
            continue;
          }
          this.error = '';
          await this.delayMs(400);
          continue;
        }
      }
    } finally {
      this.serveWorkers = Math.max(0, this.serveWorkers - 1);
      if (this.serveWorkers === 0) {
        this.serveLoop = false;
      }
      if (!this.serveAbort && this.paired && this.listening && this.destReady) {
        void this.startServeLoop();
      }
    }
  }

  async handleServeEvent(ev: ServeEventObj): Promise<void> {
    if (ev.kind === 'file') {
      const path = ev.path !== undefined ? ev.path : '';
      const name = path.substring(path.lastIndexOf('/') + 1);
      const title = name.length > 0 ? name : '文件';
      const bytes = ev.bytes !== undefined ? ev.bytes : 0;
      const detail = `已接收 · ${this.formatBytes(bytes)}`;
      let jobId = '';
      if (this.activeJobId.indexOf('recv-live-') === 0) {
        jobId = this.activeJobId;
        this.updateJob(jobId, 'done', detail, path, title);
      } else {
        jobId = `recv-${Date.now()}`;
        this.pushJob({
          id: jobId,
          kind: 'recv',
          title: title,
          detail: detail,
          status: 'done',
          path: path,
          rev: 1
        });
      }
      this.activeJobId = '';
      this.banner = `已收文件 · ${this.formatBytes(bytes)} · 正在写入保存目录…`;
      // Do not await: save must not block the next serve_once.
      void this.promptSaveRecvJob(jobId, path, title, bytes);
    } else if (ev.kind === 'clipboard_text') {
      const text = ev.text !== undefined ? ev.text : '';
      this.lastClipText = text.trim();
      const written = this.writeOsClipboard(text);
      this.clipText = text;
      this.pushJob({
        id: `clip-${Date.now()}`,
        kind: 'clip',
        title: '收到文本',
        detail: written ? text.substring(0, 48) : '接收成功但写系统剪贴板失败',
        status: written ? 'done' : 'fail'
      });
      this.banner = written ? '已收文本并写入剪贴板' : '已收文本，写剪贴板失败';
      this.clipHint = this.banner;
      if (written) {
        void this.notifyAlert(Index.NOTIF_ID_CLIP, 'LanBridge', '已收到电脑剪贴板');
      }
    } else if (ev.kind === 'clipboard_png') {
      const b64 = ev.png_base64 !== undefined ? ev.png_base64 : '';
      this.pendingInboundPng = this.base64ToArrayBuffer(b64);
      this.pendingInboundAt = Date.now();
      const written = await this.writeOsClipboardPng(this.base64ToArrayBuffer(b64));
      this.pushJob({
        id: `clip-${Date.now()}`,
        kind: 'clip',
        title: '收到图片',
        detail: written ? '已同步图片' : '接收成功但写系统剪贴板失败',
        status: written ? 'done' : 'fail'
      });
      this.banner = written ? '已收图片并写入剪贴板' : '已收图片，写剪贴板失败';
      this.clipHint = this.banner;
      if (written) {
        void this.notifyAlert(Index.NOTIF_ID_CLIP, 'LanBridge', '已收到电脑剪贴板图片');
      }
    } else if (ev.kind === 'heartbeat' || ev.kind === 'peer_closed') {
      // Core-side liveness / probe teardown: keep the UI quiet.
    } else if (ev.kind === 'peer_left') {
      this.clipHint = '对端已取消配对：请重新填 6 位码配对';
      this.banner = '对端已取消配对';
      void this.notifyAlert(Index.NOTIF_ID_PAIR, 'LanBridge', '对端已取消配对');
    } else {
      this.banner = ev.message !== undefined ? ev.message : '入站错误';
    }
    await this.refreshStatusKeepBanner();
  }

  // ======================= new UI: 4 tabs =======================
  @State activeTab: number = 0;
  /** 系统栏安全区（全屏布局下用来给内容留边）。 */
  @State topInset: number = 0;
  @State bottomInset: number = 0;

  private loadSafeArea(): void {
    try {
      window.getLastWindow(this.hostCtx()).then((win: window.Window) => {
        const avoid = win.getWindowAvoidArea(window.AvoidAreaType.TYPE_SYSTEM);
        const ui = this.getUIContext();
        this.topInset = ui.px2vp(avoid.topRect.height);
        this.bottomInset = ui.px2vp(avoid.bottomRect.height);
      }).catch(() => {
      });
    } catch (_e) {
    }
  }

  @Builder
  TabItem(label: string, idx: number) {
    Column({ space: 2 }) {
      Text(idx === 0 ? '⌂' : (idx === 1 ? '⇅' : (idx === 2 ? '⎘' : '⚙')))
        .fontSize(18)
        .fontColor(this.activeTab === idx ? $r('app.color.primary') : $r('app.color.text_tertiary'))
      Text(label)
        .fontSize(11)
        .fontColor(this.activeTab === idx ? $r('app.color.primary') : $r('app.color.text_tertiary'))
    }
    .width('100%')
    .height('100%')
    .justifyContent(FlexAlign.Center)
  }

  @Builder
  TabConnection() {
    Scroll() {
      Column({ space: 16 }) {
        if (this.paired) {
          Column({ space: 10 }) {
            Row({ space: 14 }) {
              Stack() {
                Circle({ width: 52, height: 52 }).fill($r('app.color.primary_bg'))
                Text('PC').fontSize(16).fontWeight(FontWeight.Bold).fontColor($r('app.color.primary'))
              }
              Column({ space: 4 }) {
                Text(this.peerLine.length > 0 ? this.peerLine.replace('已信任：', '') : '已配对设备')
                  .fontSize(18).fontWeight(FontWeight.Bold).fontColor($r('app.color.text_primary'))
                Text(this.connectionStateLine())
                  .fontSize(13)
                  .fontColor(this.connectionLabel === '已连接' ? $r('app.color.success') : $r('app.color.text_secondary'))
              }
              .alignItems(HorizontalAlign.Start)
              .layoutWeight(1)
            }
            .width('100%')
            Divider().color($r('app.color.divider'))
            Row({ space: 8 }) {
              Button('检测对端')
                .fontSize(14).height(36).layoutWeight(1)
                .backgroundColor($r('app.color.primary')).fontColor($r('app.color.on_primary'))
                .enabled(!this.busy)
                .onClick(() => { void this.onProbePeer(); })
              Button('取消配对')
                .fontSize(14).height(36).layoutWeight(1)
                .backgroundColor(Color.Transparent).fontColor($r('app.color.danger'))
                .border({ width: 1, color: $r('app.color.danger') })
                .enabled(!this.busy)
                .onClick(() => { void this.onClearPair(); })
            }
            .width('100%')
          }
          .width('100%').padding(18)
          .backgroundColor($r('app.color.card')).borderRadius(16)
          .border({ width: 1, color: $r('app.color.card_border') })

          if (this.jobs.length > 0) {
            Column({ space: 8 }) {
              Text('最近活动').fontSize(13).fontColor($r('app.color.text_secondary')).width('100%')
              ForEach(this.jobs.slice(0, 3), (j: JobCard) => {
                Row() {
                  Text(j.title).fontSize(13).fontColor($r('app.color.text_primary')).layoutWeight(1)
                  Text(j.status === 'done' ? '完成' : (j.status === 'fail' ? '失败' : '进行中'))
                    .fontSize(12)
                    .fontColor(j.status === 'fail' ? $r('app.color.danger') : $r('app.color.text_tertiary'))
                }.width('100%')
              }, (j: JobCard) => j.id + (j.rev !== undefined ? String(j.rev) : ''))
            }
            .width('100%').padding(16)
            .backgroundColor($r('app.color.card')).borderRadius(16)
            .border({ width: 1, color: $r('app.color.card_border') })
          }
        } else {
          Column({ space: 6 }) {
            Text('连接电脑').fontSize(22).fontWeight(FontWeight.Bold).fontColor($r('app.color.text_primary'))
            Text(this.listening ? '让电脑填这个码，或填电脑屏幕上的 6 位码' : '点下方按钮开始')
              .fontSize(13).fontColor($r('app.color.text_secondary'))
          }
          .alignItems(HorizontalAlign.Start).width('100%')

          if (!this.listening) {
            Button('开始配对')
              .width('100%').height(48)
              .backgroundColor($r('app.color.primary')).fontColor($r('app.color.on_primary'))
              .enabled(!this.busy)
              .onClick(() => { void this.onBeginPair(); })
          } else {
            Column({ space: 10 }) {
              Text('本机码（给电脑填）').fontSize(12).fontColor($r('app.color.text_secondary'))
              Text(this.pairingCode.length > 0 ? this.pairingCode : '------')
                .fontSize(40).fontWeight(FontWeight.Bold).letterSpacing(8).fontColor($r('app.color.primary'))
              if (this.localIpLine.length > 0) {
                Text('本机 IP ' + this.localIpLine).fontSize(12).fontColor($r('app.color.text_tertiary'))
              }
            }
            .width('100%').padding(18)
            .backgroundColor($r('app.color.card')).borderRadius(16)
            .border({ width: 1, color: $r('app.color.card_border') })

            TextInput({ text: this.pairCodeEdit, placeholder: '电脑屏幕上的 6 位码' })
              .onChange((v: string) => { this.pairCodeEdit = v; })
              .backgroundColor($r('app.color.input_bg')).height(44)
            Button(this.busy ? '连接中…' : '连接')
              .width('100%').height(48)
              .backgroundColor($r('app.color.primary')).fontColor($r('app.color.on_primary'))
              .enabled(!this.busy && this.pairCodeEdit.trim().length >= 6)
              .onClick(() => { void this.onConfirmPair(); })
          }

          Button(this.showManualPair ? '收起 IP 连接' : '用 IP 连接')
            .fontSize(13).height(36)
            .backgroundColor(Color.Transparent).fontColor($r('app.color.primary'))
            .onClick(() => { this.toggleManualPair(); })
          if (this.showManualPair) {
            TextInput({ text: this.peerHostEdit, placeholder: '对方 IP' })
              .onChange((v: string) => { this.peerHostEdit = v; })
              .backgroundColor($r('app.color.input_bg')).height(44)
            Button('用 IP 连接')
              .width('100%').height(44)
              .backgroundColor($r('app.color.accent')).fontColor($r('app.color.on_primary'))
              .enabled(!this.busy)
              .onClick(() => { void this.onPair(); })
          }

          Column({ space: 8 }) {
            Row() {
              Text('附近设备').fontSize(13).fontColor($r('app.color.text_secondary')).layoutWeight(1)
                Button('刷新').fontSize(12).height(28)
                  .backgroundColor(Color.Transparent).fontColor($r('app.color.primary'))
                  .onClick(() => { void this.onRefreshNearby(); })
              }.width('100%')
              if (this.nearbyList.length === 0) {
                Text('附近没有设备（热点下常见）。用上面的本机码或 IP 连接即可。')
                  .fontSize(12).fontColor($r('app.color.text_tertiary')).width('100%')
              }
              ForEach(this.nearbyList, (p: NearbyPeerObj) => {
                Row() {
                  Column({ space: 2 }) {
                    Text(p.alias.length > 0 ? p.alias : p.device_id)
                      .fontSize(15).fontColor($r('app.color.text_primary'))
                    Text(p.trusted ? '已信任' : '新设备').fontSize(12).fontColor($r('app.color.text_tertiary'))
                  }.alignItems(HorizontalAlign.Start).layoutWeight(1)
                  Button(p.trusted ? '检测' : '配对')
                    .fontSize(12).height(32)
                    .enabled(!this.busy)
                    .onClick(() => {
                      if (p.trusted) { void this.onProbePeer(); } else { void this.onPairNearby(p); }
                    })
                }.width('100%').padding({ top: 8, bottom: 8 })
            }, (p: NearbyPeerObj) => p.device_id)
          }
          .width('100%').padding(16)
          .backgroundColor($r('app.color.card')).borderRadius(16)
          .border({ width: 1, color: $r('app.color.card_border') })
        }

        if (this.error.length > 0) {
          Text(this.error).fontSize(12).fontColor($r('app.color.danger')).width('100%')
        }
      }
      .padding({ left: 18, right: 18, top: 14, bottom: 28 })
      .width('100%')
      .alignItems(HorizontalAlign.Start)
    }
    .width('100%').height('100%').scrollBar(BarState.Off)
    .align(Alignment.Top)
  }

  @Builder
  TabTransfer() {
    Scroll() {
      Column({ space: 16 }) {
        Button('＋ 添加文件 / 文件夹')
          .width('100%').height(52)
          .backgroundColor($r('app.color.card')).fontColor($r('app.color.primary'))
          .border({ width: 1, color: $r('app.color.card_border'), style: BorderStyle.Dashed })
          .onClick(() => { void this.onPickSendFile(); })

        if (this.sendQueue.length > 0) {
          Column({ space: 10 }) {
            Row() {
              Text('待发送 ' + this.sendQueue.length + ' 项').fontSize(13).fontColor($r('app.color.text_secondary')).layoutWeight(1)
              Button('清空').fontSize(12).height(28)
                .backgroundColor(Color.Transparent).fontColor($r('app.color.text_secondary'))
                .onClick(() => { this.clearSendQueue(); })
            }.width('100%')
            Button(this.busy ? '发送中…' : '发送到电脑')
              .width('100%').height(46)
              .backgroundColor($r('app.color.primary')).fontColor($r('app.color.on_primary'))
              .enabled(!this.busy && this.canOperate)
              .onClick(() => { void this.onSendFile(); })
          }
          .width('100%').padding(16)
          .backgroundColor($r('app.color.card')).borderRadius(16)
          .border({ width: 1, color: $r('app.color.card_border') })
        }

        Text('传输记录').fontSize(13).fontColor($r('app.color.text_secondary')).width('100%')
        if (this.jobs.length === 0) {
          Text('还没有记录').fontSize(13).fontColor($r('app.color.text_tertiary')).width('100%')
        } else {
          ForEach(this.jobs, (j: JobCard) => {
            Row() {
              Column({ space: 2 }) {
                Text(j.title).fontSize(14).fontColor($r('app.color.text_primary'))
                Text(j.detail).fontSize(12).fontColor($r('app.color.text_tertiary')).maxLines(1)
                  .textOverflow({ overflow: TextOverflow.Ellipsis })
              }.alignItems(HorizontalAlign.Start).layoutWeight(1)
              Text(j.status === 'done' ? '完成' : (j.status === 'fail' ? '失败' : '进行中'))
                .fontSize(12)
                .fontColor(j.status === 'fail' ? $r('app.color.danger') : $r('app.color.text_tertiary'))
            }
            .width('100%').padding({ top: 10, bottom: 10 })
            .onClick(() => { void this.openJob(j); })
          }, (j: JobCard) => j.id + (j.rev !== undefined ? String(j.rev) : ''))
        }
      }
      .padding({ left: 18, right: 18, top: 14, bottom: 28 })
      .width('100%').alignItems(HorizontalAlign.Start)
    }
    .width('100%').height('100%').scrollBar(BarState.Off)
    .align(Alignment.Top)
  }

  @Builder
  TabClipboard() {
    Scroll() {
      Column({ space: 14 }) {
        Row() {
          Column({ space: 2 }) {
            Text('自动同步剪贴板').fontSize(15).fontColor($r('app.color.text_primary'))
            Text('本机复制后自动发到电脑').fontSize(12).fontColor($r('app.color.text_tertiary'))
          }.alignItems(HorizontalAlign.Start).layoutWeight(1)
          Toggle({ type: ToggleType.Switch, isOn: this.clipAutoOk })
            .selectedColor($r('app.color.primary'))
            .onChange((on: boolean) => {
              if (on) { void this.ensureClipAutoSilent(); }
              else { this.stopClipWatch(); this.clipAutoOk = false; }
            })
        }
        .width('100%').padding(16)
        .backgroundColor($r('app.color.card')).borderRadius(16)
        .border({ width: 1, color: $r('app.color.card_border') })

        Row() {
          Column({ space: 2 }) {
            Text('截图即共享').fontSize(15).fontColor($r('app.color.text_primary'))
            Text(this.shotHint.length > 0 ? this.shotHint : '新截图自动发到电脑').fontSize(12)
              .fontColor($r('app.color.text_tertiary')).maxLines(2)
          }.alignItems(HorizontalAlign.Start).layoutWeight(1)
          Toggle({ type: ToggleType.Switch, isOn: this.shotAuto })
            .selectedColor($r('app.color.primary'))
            .onChange((_on: boolean) => { void this.toggleShotAuto(); })
        }
        .width('100%').padding(16)
        .backgroundColor($r('app.color.card')).borderRadius(16)
        .border({ width: 1, color: $r('app.color.card_border') })

        Text(this.clipHint)
          .fontSize(12).fontColor($r('app.color.text_tertiary')).width('100%')

        Button('发送当前剪贴板')
          .width('100%').height(46)
          .backgroundColor($r('app.color.primary')).fontColor($r('app.color.on_primary'))
          .enabled(!this.busy && this.canOperate)
          .onClick(() => { void this.onSendClipText(); })
      }
      .padding({ left: 18, right: 18, top: 14, bottom: 28 })
      .width('100%').alignItems(HorizontalAlign.Start)
    }
    .width('100%').height('100%').scrollBar(BarState.Off)
    .align(Alignment.Top)
  }

  @Builder
  TabSettings() {
    Scroll() {
      Column({ space: 10 }) {
        Row() {
          Text('保存位置').fontSize(15).fontColor($r('app.color.text_primary')).layoutWeight(1)
          Text(this.destLabel.length > 0 ? this.destLabel : '未选择')
            .fontSize(13).fontColor($r('app.color.text_secondary')).maxLines(1)
            .textOverflow({ overflow: TextOverflow.Ellipsis }).layoutWeight(1)
          Text('>').fontSize(13).fontColor($r('app.color.text_tertiary'))
        }
        .width('100%').padding(16)
        .backgroundColor($r('app.color.card')).borderRadius(16)
        .border({ width: 1, color: $r('app.color.card_border') })
        .onClick(() => { void this.onPickRecvDest(); })

        Row() {
          Text('主题').fontSize(15).fontColor($r('app.color.text_primary')).layoutWeight(1)
          Text(this.themePref === 'system' ? '跟随系统' : (this.themePref === 'dark' ? '深色' : '浅色'))
            .fontSize(13).fontColor($r('app.color.text_secondary'))
        }
        .width('100%').padding(16)
        .backgroundColor($r('app.color.card')).borderRadius(16)
        .border({ width: 1, color: $r('app.color.card_border') })
        .onClick(() => {
          const next: ThemePref = this.themePref === 'system' ? 'light' : (this.themePref === 'light' ? 'dark' : 'system');
          this.applyTheme(next);
        })

        Row() {
          Text('后台保活').fontSize(15).fontColor($r('app.color.text_primary')).layoutWeight(1)
          Text(this.bgTaskOk ? '已开启' : '未开启').fontSize(13)
            .fontColor(this.bgTaskOk ? $r('app.color.success') : $r('app.color.text_tertiary'))
        }
        .width('100%').padding(16)
        .backgroundColor($r('app.color.card')).borderRadius(16)
        .border({ width: 1, color: $r('app.color.card_border') })

        Button('清除配对')
          .width('100%').height(44)
          .backgroundColor($r('app.color.danger_bg')).fontColor($r('app.color.danger'))
          .enabled(!this.busy)
          .onClick(() => { void this.onClearPair(); })

        Text('LanBridge 0.1.0 · 局域网直连，无云中继')
          .fontSize(12).fontColor($r('app.color.text_tertiary')).width('100%').margin({ top: 8 })
      }
      .padding({ left: 18, right: 18, top: 14, bottom: 28 })
      .width('100%').alignItems(HorizontalAlign.Start)
    }
    .width('100%').height('100%').scrollBar(BarState.Off)
    .align(Alignment.Top)
  }

  build() {
    Column() {
      Row({ space: 8 }) {
        Text('LanBridge').fontSize(16).fontWeight(FontWeight.Bold).fontColor($r('app.color.text_primary'))
        Blank()
        Circle({ width: 8, height: 8 })
          .fill(this.connectionLabel === '已连接' ? $r('app.color.success') :
            (this.connectionLabel === '等待配对' ? $r('app.color.warning') : $r('app.color.text_tertiary')))
        Text(this.connectionLabel)
          .fontSize(12)
          .fontColor(this.connectionLabel === '已连接' ? $r('app.color.success') : $r('app.color.text_secondary'))
      }
      .width('100%')
      .padding({ left: 18, right: 18, top: 10 + (this.topInset > 0 ? this.topInset : 30), bottom: 10 })

      Tabs({ barPosition: BarPosition.End, index: this.activeTab }) {
        TabContent() { this.TabConnection() }.tabBar(this.TabItem('连接', 0))
        TabContent() { this.TabTransfer() }.tabBar(this.TabItem('传输', 1))
        TabContent() { this.TabClipboard() }.tabBar(this.TabItem('剪贴板', 2))
        TabContent() { this.TabSettings() }.tabBar(this.TabItem('设置', 3))
      }
      .layoutWeight(1)
      .barHeight(58)
      .padding({ bottom: this.bottomInset > 0 ? this.bottomInset : 24 })
      .onChange((index: number) => { this.activeTab = index; })
    }
    .width('100%')
    .height('100%')
    .expandSafeArea([SafeAreaType.SYSTEM], [SafeAreaEdge.TOP, SafeAreaEdge.BOTTOM])
    .backgroundColor($r('app.color.page_bg'))
  }

}