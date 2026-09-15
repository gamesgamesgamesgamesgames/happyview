"use client";

import { X } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

// A growing list of single-line values: typing into the last row adds a
// row, clearing a row removes it, so the operator never manages the list
// length by hand.
export function MultiInput({
  values,
  onChange,
  placeholder,
  readonlyValues = [],
  id,
  disabled = false,
}: {
  values: string[];
  onChange: (values: string[]) => void;
  placeholder?: string;
  readonlyValues?: string[];
  id?: string;
  disabled?: boolean;
}) {
  function handleChange(index: number, value: string) {
    const next = [...values];
    next[index] = value;
    // If user typed into the last input, add an empty one
    if (index === values.length - 1 && value.trim() !== "") {
      next.push("");
    }
    onChange(next);
  }

  function handleRemove(index: number) {
    const next = values.filter((_, i) => i !== index);
    // Always keep at least one empty input
    if (next.length === 0 || next[next.length - 1].trim() !== "") {
      next.push("");
    }
    onChange(next);
  }

  // A pasted list becomes rows rather than one row holding the whole list:
  // the values come from a config file or another form far more often than
  // one is typed by hand.
  function handlePaste(index: number, e: React.ClipboardEvent<HTMLInputElement>) {
    const parts = e.clipboardData
      .getData("text")
      .split(/[,;\s]+/)
      .map((part) => part.trim())
      .filter(Boolean);
    if (parts.length <= 1) return;
    e.preventDefault();
    const before = values.slice(0, index);
    const after = values.slice(index + 1).filter((value) => value.trim() !== "");
    onChange([...before, ...parts, ...after, ""]);
  }

  function handleKeyDown(
    index: number,
    e: React.KeyboardEvent<HTMLInputElement>,
  ) {
    if (e.key === "Backspace" && values[index] === "" && values.length > 1) {
      e.preventDefault();
      handleRemove(index);
    }
  }

  return (
    <div className="flex flex-col gap-1.5">
      {readonlyValues.map((val, i) => (
        <Input
          key={`readonly-${i}`}
          value={val}
          readOnly
          className="font-mono text-sm bg-muted"
        />
      ))}
      {values.map((val, index) => (
        <div key={index} className="flex gap-1.5">
          <Input
            id={index === 0 ? id : undefined}
            value={val}
            onChange={(e) => handleChange(index, e.target.value)}
            onKeyDown={(e) => handleKeyDown(index, e)}
            onPaste={(e) => handlePaste(index, e)}
            placeholder={placeholder}
            className="font-mono text-sm"
            disabled={disabled}
          />
          {!disabled && values.length > 1 && val.trim() !== "" && (
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="shrink-0 size-9 text-muted-foreground hover:text-destructive"
              onClick={() => handleRemove(index)}
            >
              <X className="size-4" />
            </Button>
          )}
        </div>
      ))}
    </div>
  );
}
