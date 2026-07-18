import type { AcpPermissionRequest } from "../acp/client";

interface Props {
  request: AcpPermissionRequest;
  onSelect: (optionId: string | null) => void;
}

export function PermissionCard({ request, onSelect }: Props) {
  return (
    <div className="permission-card">
      <div className="permission-header">
        <strong>Permission requested</strong>
        <p>{request.toolCall.title || request.toolCall.command || "Tool call"}</p>
      </div>
      <div className="permission-options">
        {request.options.map((opt) => (
          <button key={opt.optionId} className="permission-option" onClick={() => onSelect(opt.optionId)}>
            <span>{opt.name}</span>
            {opt.kind ? <span className="option-kind">{opt.kind}</span> : null}
          </button>
        ))}
        <button className="permission-option permission-cancel" onClick={() => onSelect(null)}>
          <span>Cancel</span>
        </button>
      </div>
    </div>
  );
}
