import {Children, isValidElement} from "react";
import type {ReactNode} from "react";
import {RustSurface} from "./rust-surface.js";

interface Option {value: string; label: string; disabled: boolean}
type ValueChange = (event: {target: {value: string}}) => void;

function optionList(children: ReactNode): Option[] {
  return Children.toArray(children).map(child => {
    if (!isValidElement<{value: string | number; children: ReactNode; disabled?: boolean}>(child) || child.type !== "option")
      throw new Error("Interpreter choices require labeled options.");
    return {value: String(child.props.value), disabled: !!child.props.disabled,
      label: Children.toArray(child.props.children).map(String).join("")};
  });
}

interface SelectProps {
  id?: string; value: string | number; children: ReactNode; disabled?: boolean;
  "aria-describedby"?: string; onChange: ValueChange;
}
export function RustSelect({children, onChange, ...props}: SelectProps) {
  return <RustSurface kind="select" state={{...props, value: String(props.value), options: optionList(children)}}
    dispatch={(_, value) => onChange({target: {value}})}
    fallback={<select {...props} onChange={onChange}>{children}</select>}/>;
}

interface TextProps {
  id?: string; value: string; rows?: number; maxLength?: number; placeholder?: string;
  "aria-describedby"?: string; "aria-invalid"?: boolean; onChange: ValueChange;
}
export function RustTextarea({onChange, ...props}: TextProps) {
  return <RustSurface kind="textarea" state={props}
    dispatch={(_, value) => onChange({target: {value}})}
    fallback={<textarea {...props} onChange={onChange}/>}/>;
}

interface InputProps {
  id?: string; name?: string; value?: string | number; type: "checkbox" | "radio" | "number" | "range";
  checked?: boolean; disabled?: boolean; min?: number | string; max?: number | string; step?: number | string;
  "aria-invalid"?: boolean; onChange(event: {target: {value: string; checked: boolean}}): void;
}
export function RustInput({onChange, ...props}: InputProps) {
  const checkedInput = props.type === "checkbox" || props.type === "radio";
  return <RustSurface kind="input" state={props} dispatch={(_, value) => {
    onChange({target: {value: checkedInput ? String(props.value ?? "on") : value, checked: checkedInput && value === "true"}});
  }} fallback={<input {...props} onChange={onChange}/>}/>;
}
