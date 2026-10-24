// Text edited where it is shown: a page title (click it) and a body (the
// pencil in its header strip).

import { ActionIcon, Button, Group, Stack, Textarea, TextInput, Title } from "@mantine/core";
import { Pencil } from "lucide-react";
import { useState, type ReactNode } from "react";
import { useT } from "../i18n";

/** A page title that turns into a field when clicked; `onSave` resolves when saved. */
export function EditableTitle({
  prefix,
  value,
  onSave,
  pending,
}: {
  prefix: ReactNode;
  value: string;
  onSave: (title: string) => Promise<unknown>;
  pending: boolean;
}) {
  const t = useT();
  const [editing, setEditing] = useState<string | null>(null);
  if (editing === null) {
    return (
      <Title
        order={2}
        className="editable-title"
        tabIndex={0}
        title={t("edit.clickToEdit")}
        onClick={() => setEditing(value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            setEditing(value);
          }
        }}
      >
        <span className="page-key">{prefix}</span> {value}
        <Pencil size={18} className="pencil" aria-hidden />
      </Title>
    );
  }
  const save = () => void onSave(editing.trim()).then(() => setEditing(null), () => undefined);
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      <Group gap="xs" wrap="nowrap">
        <span className="page-key" style={{ fontSize: 26 }}>
          {prefix}
        </span>
        <TextInput
          aria-label={t("ticket.title")}
          value={editing}
          onChange={(e) => setEditing(e.currentTarget.value)}
          onKeyDown={(e) => e.key === "Escape" && setEditing(null)}
          size="md"
          w={560}
          required
          autoFocus
        />
        <Button variant="default" onClick={() => setEditing(null)}>
          {t("common.cancel")}
        </Button>
        <Button type="submit" disabled={pending || !editing.trim()}>
          {t("common.save")}
        </Button>
      </Group>
    </form>
  );
}

/** The pencil of a body's header strip. */
export function EditPencil({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <ActionIcon variant="subtle" color="gray" ml="auto" aria-label={label} onClick={onClick}>
      <Pencil size={16} />
    </ActionIcon>
  );
}

/** A body's Markdown field with Cancel and Save. */
export function BodyEditor({
  value,
  onSave,
  onCancel,
  pending,
  label,
  required = false,
}: {
  value: string;
  onSave: (body: string) => void;
  onCancel: () => void;
  pending: boolean;
  label?: string;
  required?: boolean;
}) {
  const t = useT();
  const [body, setBody] = useState(value);
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        if (!pending && (!required || body.trim())) onSave(body);
      }}
    >
      <Stack gap="xs">
        <Textarea aria-label={label ?? t("ticket.body")} value={body} onChange={(e) => setBody(e.currentTarget.value)} rows={8} required={required} autoFocus />
        <Group justify="flex-end" gap="xs">
          <Button variant="default" onClick={onCancel} disabled={pending}>
            {t("common.cancel")}
          </Button>
          <Button type="submit" disabled={pending || (required && !body.trim())}>
            {t("common.save")}
          </Button>
        </Group>
      </Stack>
    </form>
  );
}
