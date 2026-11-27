import {Children} from "react";
import type {ReactNode} from "react";
import {RustSurface} from "./rust-surface.js";

interface Props {
  children: ReactNode;
  id?: string;
  className?: string;
  disabled?: boolean;
  type?: "button" | "submit";
  "aria-expanded"?: boolean;
  "aria-controls"?: string;
  "aria-pressed"?: boolean;
  "aria-describedby"?: string;
  onClick?(): void;
}

/** Preserve native submit, keyboard and fieldset behavior for advanced controls. */
export function RustButton({children, onClick, ...props}: Props) {
  const label = Children.toArray(children).map(part => {
    if (typeof part !== "string" && typeof part !== "number") throw new Error("Interpreter action labels must be text.");
    return String(part);
  }).join("");
  return <RustSurface kind="button" state={{...props, label}}
    dispatch={() => {if (!props.disabled) onClick?.();}}
    fallback={<button {...props} onClick={onClick}>{children}</button>}/>;
}
