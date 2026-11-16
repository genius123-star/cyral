/**
 * 自由活动动画通道：manifest.json 驱动 + 放图即用。
 *
 * 接入手册（也见 docs/HANDOFF.md）：
 * 1. 把帧条 PNG 放入 public/assets/characters/cyral/，文件名与 manifest.json 中
 *    `strip` 末尾一致（如 walk-row.png、compact.png）。
 * 2. 若帧数/帧尺寸/时长与 manifest 中的占位默认不同，改 manifest.json 对应字段；
 *    否则放图即可用。
 * 3. 图片加载成功才会进入可用集合；缺失/加载失败自动回退眨眼待机，不会报错。
 */

export interface PetActionSpec {
  action: string;
  strip: string;
  frames: number;
  frameWidth: number;
  frameHeight: number;
  durationMs: number;
  loop: boolean;
  /** 非循环动作播完后接续的动作名。 */
  nextAction?: string;
  /** walk 为移动必需资源：缺失时 Rust 关闭自动游走。 */
  required?: boolean;
  /** 帧条在视口内的纵向偏移（px），兼容旧资产留白。 */
  offsetY?: number;
  flipX?: boolean;
}

export interface PetManifest {
  version: number;
  actions: PetActionSpec[];
}

export const PET_MANIFEST_URL = "/assets/characters/cyral/manifest.json";
export const IDLE_ACTION = "idle-blink";
export const IDLE_ACTIONS = [IDLE_ACTION] as const;
export const WALK_ACTION = "walk-a";
export const WALK_ACTIONS = [WALK_ACTION, "walk-b"] as const;
export const COMPACT_ACTION = "compact";
export const ENTER_ACTION = "enter";
export const EXIT_ACTION = "exit";
export const PEEK_ACTIONS = ["peek-left", "peek-right", "peek-bottom", "peek-top"] as const;

/** manifest 或图片缺失时的最终兜底：眨眼待机帧条，保证永不白屏。 */
export const IDLE_FALLBACK: PetActionSpec = {
  action: IDLE_ACTION,
  strip: "/assets/characters/cyral/idle-blink-row-v1.png",
  frames: 4,
  frameWidth: 180,
  frameHeight: 226,
  durationMs: 1000,
  loop: true,
};

export async function loadPetManifest(
  fetchImpl: typeof fetch = fetch,
  url: string = PET_MANIFEST_URL,
): Promise<PetManifest> {
  const response = await fetchImpl(url);
  if (!response.ok) {
    throw new Error(`动画 manifest 加载失败：HTTP ${response.status}`);
  }
  return (await response.json()) as PetManifest;
}

/** 预加载所有帧条：加载成功才进入可用集合，失败静默剔除（回退眨眼待机）。 */
export async function preloadActionImages(
  specs: PetActionSpec[],
  createImage: (src: string) => HTMLImageElement = (src) => {
    const image = new Image();
    image.src = src;
    return image;
  },
): Promise<Set<string>> {
  const results = await Promise.all(
    specs.map(
      (spec) =>
        new Promise<string | null>((resolve) => {
          const image = createImage(spec.strip);
          image.onload = () => resolve(spec.action);
          image.onerror = () => resolve(null);
        }),
    ),
  );
  return new Set(results.filter((action): action is string => action !== null));
}

export function actionSpec(manifest: PetManifest, action: string): PetActionSpec | undefined {
  return manifest.actions.find((spec) => spec.action === action);
}

/** 解析当前动作：优先可用原动作，其次眨眼待机，最后硬编码兜底。 */
export function resolveAction(
  action: string,
  available: Set<string>,
  manifest?: PetManifest,
): PetActionSpec {
  if (manifest) {
    const requested = actionSpec(manifest, action);
    if (requested && available.has(requested.action)) {
      return requested;
    }
    const idle = actionSpec(manifest, IDLE_ACTION);
    if (idle && available.has(idle.action)) {
      return idle;
    }
  }
  return IDLE_FALLBACK;
}
