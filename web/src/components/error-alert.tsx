import { CircleAlert } from "lucide-react";
import { ApiError } from "@/api/client";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";

export function describeError(error: unknown): string {
	if (error instanceof ApiError) {
		return `${error.message} (${error.code}, HTTP ${error.status})`;
	}
	return error instanceof Error ? error.message : String(error);
}

export function ErrorAlert({ title, error }: { title: string; error: unknown }) {
	return (
		<Alert variant='destructive'>
			<CircleAlert />
			<AlertTitle>{title}</AlertTitle>
			<AlertDescription>{describeError(error)}</AlertDescription>
		</Alert>
	);
}
