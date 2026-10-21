import { AbilityConstant, ConfigurationConstant, UIAbility, Want } from '@kit.AbilityKit';
import { BusinessError } from '@kit.BasicServicesKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { window } from '@kit.ArkUI';
import { systemShare } from '@kit.ShareKit';

const DOMAIN = 0x0000;
/** Single-record share (gallery/file one item). */
const SHARE_ACTION = 'ohos.want.action.sendData';
/** Multi-record share — system uses this when user picks multiple files. */
const SHARE_MULTI_ACTION = 'ohos.want.action.sendMultipleData';

/** AppStorage payload written by share ingest; consumed by Index. */
interface LbSharePendingPayload {
  texts: string[];
  uris: string[];
  ts: number;
}

export default class EntryAbility extends UIAbility {
  private mainWindow: window.Window | undefined = undefined;

  onCreate(want: Want, _launchParam: AbilityConstant.LaunchParam): void {
    hilog.info(DOMAIN, 'LanBridge', 'Ability onCreate');
    AppStorage.setOrCreate('lbForeground', true);
    AppStorage.setOrCreate('lbClipDirty', false);
    AppStorage.setOrCreate('lbSharePendingJson', '');
    AppStorage.setOrCreate('lbSharePendingRev', 0);
    this.ingestShareWant(want);
  }

  onNewWant(want: Want, _launchParam: AbilityConstant.LaunchParam): void {
    hilog.info(DOMAIN, 'LanBridge', 'Ability onNewWant');
    this.ingestShareWant(want);
  }

  onDestroy(): void {
    hilog.info(DOMAIN, 'LanBridge', 'Ability onDestroy');
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    hilog.info(DOMAIN, 'LanBridge', 'Ability onWindowStageCreate');
    windowStage.getMainWindow().then((win: window.Window) => {
      this.mainWindow = win;
      this.applySystemBars();
      this.setKeepScreenOn(true);
    }).catch((err: BusinessError) => {
      hilog.error(DOMAIN, 'LanBridge', 'getMainWindow failed %{public}d %{public}s', err.code, err.message);
    });
    windowStage.loadContent('pages/Index', (err) => {
      if (err.code) {
        hilog.error(DOMAIN, 'LanBridge', 'Failed to load content %{public}d', err.code);
        return;
      }
      hilog.info(DOMAIN, 'LanBridge', 'Succeeded in loading content');
    });
  }

  onWindowStageDestroy(): void {
    this.mainWindow = undefined;
    hilog.info(DOMAIN, 'LanBridge', 'Ability onWindowStageDestroy');
  }

  onForeground(): void {
    hilog.info(DOMAIN, 'LanBridge', 'Ability onForeground');
    AppStorage.setOrCreate('lbForeground', true);
    this.setKeepScreenOn(true);
  }

  onBackground(): void {
    hilog.info(DOMAIN, 'LanBridge', 'Ability onBackground');
    AppStorage.setOrCreate('lbForeground', false);
    this.setKeepScreenOn(false);
  }

  /**
   * Share Kit target path (vendor: hms-systemShare-excerpt.md).
   * Failures are logged only — never terminateSelf on the main EntryAbility.
   */
  private isShareAction(action: string): boolean {
    return action === SHARE_ACTION || action === SHARE_MULTI_ACTION;
  }

  private ingestShareWant(want: Want): void {
    const action = want.action !== undefined && want.action !== null ? want.action : '';
    const hasUri = want.uri !== undefined && want.uri !== null && want.uri.length > 0;
    if (!this.isShareAction(action)) {
      // Non-share launch (home / viewData) — ignore for Wave B.
      return;
    }
    systemShare.getSharedData(want).then((data: systemShare.SharedData) => {
      const texts: string[] = [];
      const uris: string[] = [];
      const records = data.getRecords();
      for (let i = 0; i < records.length; i++) {
        const r = records[i];
        const uri = r.uri !== undefined && r.uri !== null ? r.uri : '';
        const content = r.content !== undefined && r.content !== null ? r.content : '';
        if (uri.length > 0) {
          uris.push(uri);
        } else if (content.length > 0) {
          texts.push(content);
        }
      }
      if (uris.length === 0 && hasUri) {
        uris.push(want.uri as string);
      }
      if (texts.length === 0 && uris.length === 0) {
        hilog.warn(DOMAIN, 'LanBridge', 'share want had no records');
        return;
      }
      this.publishSharePending(texts, uris);
    }).catch((err: BusinessError) => {
      hilog.error(
        DOMAIN,
        'LanBridge',
        'getSharedData failed %{public}d %{public}s',
        err.code,
        err.message
      );
      // Fallback: single uri on Want (file-share style without Share Kit payload).
      if (hasUri) {
        this.publishSharePending([], [want.uri as string]);
      }
    });
  }

  private publishSharePending(texts: string[], uris: string[]): void {
    const payload: LbSharePendingPayload = {
      texts: texts,
      uris: uris,
      ts: Date.now()
    };
    const json = JSON.stringify(payload);
    AppStorage.setOrCreate('lbSharePendingJson', json);
    AppStorage.set('lbSharePendingJson', json);
    const prev = AppStorage.get<number>('lbSharePendingRev');
    const next = (prev !== undefined && prev !== null ? prev : 0) + 1;
    AppStorage.setOrCreate('lbSharePendingRev', next);
    AppStorage.set('lbSharePendingRev', next);
    hilog.info(
      DOMAIN,
      'LanBridge',
      'share pending texts=%{public}d uris=%{public}d rev=%{public}d',
      texts.length,
      uris.length,
      next
    );
  }


  /** 全屏布局 + 透明系统栏（参考 harmony-tailnet 的做法）。 */
  private applySystemBars(): void {
    const win = this.mainWindow;
    if (!win) {
      return;
    }
    const dark = this.context.config.colorMode === ConfigurationConstant.ColorMode.COLOR_MODE_DARK;
    win.setWindowLayoutFullScreen(true).catch((err: BusinessError) => {
      hilog.warn(DOMAIN, 'LanBridge', 'setWindowLayoutFullScreen failed %{public}d', err.code);
    });
    win.setWindowSystemBarProperties({
      statusBarColor: '#00000000',
      isStatusBarLightIcon: dark,
      statusBarContentColor: dark ? '#FFFFFF' : '#221436',
      navigationBarColor: '#00000000',
      isNavigationBarLightIcon: dark,
      navigationBarContentColor: dark ? '#FFFFFF' : '#221436'
    }).catch((err: BusinessError) => {
      hilog.warn(DOMAIN, 'LanBridge', 'setWindowSystemBarProperties failed %{public}d', err.code);
    });
  }

  private setKeepScreenOn(on: boolean): void {
    const win = this.mainWindow;
    if (!win) {
      return;
    }
    win.setWindowKeepScreenOn(on).then(() => {
      hilog.info(DOMAIN, 'LanBridge', 'setWindowKeepScreenOn %{public}s', on ? 'true' : 'false');
    }).catch((err: BusinessError) => {
      hilog.error(
        DOMAIN,
        'LanBridge',
        'setWindowKeepScreenOn failed %{public}d %{public}s',
        err.code,
        err.message
      );
    });
  }
}
