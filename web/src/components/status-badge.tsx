import { Badge } from "@/components/ui/badge";

/** A probe/health status as a coloured badge with a text label (never colour alone). */
export function StatusBadge({ ok, label }: { ok: boolean; label: string }) {
	return (
		<Badge variant={ok ? "default" : "destructive"} data-status={ok ? "ok" : "failing"}>
			{label}
		</Badge>
	);
}
