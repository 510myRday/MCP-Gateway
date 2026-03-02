import { useRef } from "react";
import { AlignLeft } from "lucide-react";

// ── JsonEditor 组件（无高亮，带格式化按钮）───────────────────────
interface Props {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  onFormatError?: (msg: string) => void;
  formatBtnText?: string;
}

export default function JsonEditor({ value, onChange, placeholder, onFormatError, formatBtnText }: Props) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const handleFormat = () => {
    try {
      const parsed = JSON.parse(value);
      const formatted = JSON.stringify(parsed, null, 2);
      onChange(formatted);
    } catch {
      onFormatError?.("JSON 格式化失败，请检查语法");
    }
  };

  return (
    <div className="json-editor-container">
      <button
        className="json-format-btn"
        onClick={handleFormat}
        title={formatBtnText ?? "格式化 JSON"}
        type="button"
      >
        <AlignLeft size={14} />
      </button>
      <textarea
        ref={textareaRef}
        className="json-textarea"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        spellCheck={false}
        placeholder={placeholder}
        autoComplete="off"
        autoCorrect="off"
        autoCapitalize="off"
      />
    </div>
  );
}

