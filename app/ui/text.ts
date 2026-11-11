// 文字尺度:跟随「正文字号」偏好的主题 token(见 store/prefs.ts)。
// 页面里写 `class={`${FS.sm} text-foreground`}`,不要直接用 text-sm / text-xs。

export const FS = {
  "2xs": "text-(length:--fs-2xs)",
  xs: "text-(length:--fs-xs)",
  sm: "text-(length:--fs-sm)",
  base: "text-(length:--fs-base)",
  lg: "text-(length:--fs-lg)",
  xl: "text-(length:--fs-xl)",
} as const;

/** 等宽字体(SQL、单元格值、连接串)。 */
export const MONO = "font-mono";
