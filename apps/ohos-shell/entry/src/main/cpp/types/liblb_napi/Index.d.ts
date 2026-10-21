export interface IdentityView {
  deviceId: string;
  alias: string;
  fingerprintHex: string;
}

export interface HostReady {
  port: number;
  pairingCode: string;
  paired: boolean;
}

export interface GuestStatus {
  paired: boolean;
  host: string;
  port: number;
  peerId: string;
}

export interface TransferView {
  bytesTransferred: number;
  path: string;
}

export interface ClipboardRecv {
  kind: 'text' | 'png';
  text?: string;
  png?: ArrayBuffer;
}

export function protocolVersion(): number;
export function featuresLabel(): string;
export function makePairingCode(): string;
export function makeIdentity(alias: string): IdentityView;
/** Long native work: Promise (napi_async_work). Do not call sync on UI thread. */
export function selfLoopbackTransfer(): Promise<number>;

/** @deprecated prefer session_* */
export function hostStart(dataDir: string, alias: string): HostReady;
export function hostAwaitPair(): Promise<string>;
export function hostStop(): void;
export function hostRecvFile(destRoot: string): Promise<TransferView>;
export function hostRecvClipboardText(): Promise<string>;
export function hostRecvClipboard(): Promise<ClipboardRecv>;
export function guestOpen(dataDir: string, alias: string): GuestStatus;
export function guestPair(host: string, port: number, code: string): Promise<void>;
export function guestUpdateEndpoint(host: string, port: number): void;
export function guestSendFile(filePath: string, remoteName: string): Promise<number>;
export function guestSendClipboardText(text: string): Promise<void>;
export function guestSendClipboardPng(png: ArrayBuffer): Promise<void>;
export function selfLoopbackClipboardText(text: string): Promise<string>;
export function selfLoopbackClipboardPng(png: ArrayBuffer): Promise<ArrayBuffer>;

/** session-ux-v1: status/event JSON strings (parse in ArkTS). */
export function sessionOpen(dataDir: string, deviceId: string, alias: string): string;
/** Fast poll: sync try_lock + live progress atomics (safe on UI thread). */
export function sessionStatus(): string;
export function sessionGoOnline(port: number): Promise<string>;
export function sessionGoOffline(): string;
/** Bye the peer, then wipe trust. Promise — the Bye round trip must stay off the UI thread. */
export function sessionUnpair(): Promise<string>;
export function sessionBeginPair(): Promise<string>;
export function sessionPair(host: string, port: number, code: string): Promise<string>;
export function sessionSetPeer(host: string, port: number): string;
/** TLS probe trusted peer; refreshes live status. Promise (napi_async_work). */
export function sessionProbePeer(): Promise<string>;
export function sessionSendFile(filePath: string, remoteName: string): Promise<number>;
export function sessionSendClipboardText(text: string): Promise<void>;
export function sessionSendClipboardPng(png: ArrayBuffer): Promise<void>;
export function sessionServeOnce(destRoot: string): Promise<string>;
export function sessionPairingCode(): string;
