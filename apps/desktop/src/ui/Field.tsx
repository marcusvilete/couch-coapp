// Form fields: a label over its control, a hint and an error under it. The
// error line is reserved (`reserveError`) where errors come and go as you
// type, so the dialog never jumps.
import { Eye, EyeOff } from "lucide-react";
import { forwardRef, useId, useState, type InputHTMLAttributes, type ReactNode, type TextareaHTMLAttributes } from "react";
import { IconButton } from "./Button";
import "./Field.css";

export function Field(props: {
  label: ReactNode;
  hint?: ReactNode;
  error?: string | null;
  /** Keeps a line for the error even without one. */
  reserveError?: boolean;
  children: (id: string) => ReactNode;
  className?: string;
}) {
  const id = useId();
  return (
    <div className={["field", props.className ?? ""].join(" ")}>
      <label className="field-label" htmlFor={id}>
        {props.label}
      </label>
      {props.children(id)}
      {props.hint && <p className="field-hint">{props.hint}</p>}
      {(props.error || props.reserveError) && (
        <p className="field-error" role={props.error ? "alert" : undefined}>
          {props.error ?? " "}
        </p>
      )}
    </div>
  );
}

export const TextInput = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement>>(function TextInput(
  { className, ...rest },
  ref,
) {
  return <input ref={ref} className={["input", className ?? ""].join(" ")} spellCheck={false} {...rest} />;
});

export const TextArea = forwardRef<HTMLTextAreaElement, TextareaHTMLAttributes<HTMLTextAreaElement>>(
  function TextArea({ className, ...rest }, ref) {
    return <textarea ref={ref} className={["input textarea", className ?? ""].join(" ")} spellCheck={false} {...rest} />;
  },
);

/** A password with its own show/hide button. WebView2's built-in reveal
 * button hides for good once the field loses focus, so it is turned off. */
export function PasswordInput(props: {
  id?: string;
  inputRef?: React.Ref<HTMLInputElement>;
  value: string;
  onChange: (value: string) => void;
  autoComplete?: string;
  disabled?: boolean;
  autoFocus?: boolean;
  placeholder?: string;
}) {
  const [shown, setShown] = useState(false);
  return (
    <span className="password">
      <input
        id={props.id}
        ref={props.inputRef}
        className="input"
        type={shown ? "text" : "password"}
        autoComplete={props.autoComplete ?? "off"}
        spellCheck={false}
        value={props.value}
        disabled={props.disabled}
        autoFocus={props.autoFocus}
        placeholder={props.placeholder}
        onChange={(e) => props.onChange(e.target.value)}
      />
      <IconButton
        className="password-reveal"
        size="sm"
        label={shown ? "Hide password" : "Show password"}
        aria-pressed={shown}
        disabled={props.disabled}
        onClick={() => setShown(!shown)}
      >
        {shown ? <EyeOff /> : <Eye />}
      </IconButton>
    </span>
  );
}

/** A checkbox with its label beside it. */
export function Check(props: {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  title?: string;
  children: ReactNode;
}) {
  return (
    <label className={props.disabled ? "check disabled" : "check"} title={props.title}>
      <input
        type="checkbox"
        checked={props.checked}
        disabled={props.disabled}
        onChange={(e) => props.onChange(e.target.checked)}
      />
      <span>{props.children}</span>
    </label>
  );
}
