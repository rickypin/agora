import { useDialogFocus } from "./useDialogFocus";
interface Props {
  title: string;
  body: string;
  confirmLabel: string;
  onConfirm: () => void;
  onCancel: () => void;
}

/** 危险操作确认框（docs/spec/ux.md）；只有节点说"会杀"时才出现（MISSION §8）。 */
export function ConfirmDialog({ title, body, confirmLabel, onConfirm, onCancel }: Props) {
  const dialogRef = useDialogFocus(onCancel);
  return (
    <div className="overlay" role="presentation" onClick={onCancel}>
      <div className="dialog" ref={dialogRef} tabIndex={-1} role="dialog" aria-modal="true" aria-labelledby="confirm-title" onClick={(e) => e.stopPropagation()}>
        <h2 id="confirm-title">{title}</h2>
        <p>{body}</p>
        <div className="dialog-actions">
          <button onClick={onCancel} autoFocus>
            Cancel
          </button>
          <button className="danger" onClick={onConfirm}>
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
