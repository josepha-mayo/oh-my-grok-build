import type { AcpPermissionRequest } from "../acp/client";

interface Props {
  request: AcpPermissionRequest;
  onSelect: (optionId: string) => void;
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
      </div>
    </div>
  );
}
