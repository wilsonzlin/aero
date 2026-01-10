export type InputEventKind =
  | "keydown"
  | "keyup"
  | "pointerdown"
  | "pointerup"
  | "pointermove"
  | "wheel"
  | "contextmenu"
  | "unknown";

export interface HostInputEvent {
  id: number;
  kind: InputEventKind;
  t_capture_ms: number;
  payload?: unknown;
}
