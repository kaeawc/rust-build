import { createFileRoute } from "@tanstack/react-router";
import { useEffect, useId, useRef, useState } from "react";
import { useServices } from "@/api/context";
import { StatusBadge } from "@/components/status-badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { EchoSocket, sameOriginWsUrl } from "@/ws/socket";
import type { SocketStatus } from "@/ws/socket";

interface LogLine {
	id: number;
	direction: "sent" | "received";
	text: string;
}

function WsPage() {
	const { timer, createSocket } = useServices();
	const [status, setStatus] = useState<SocketStatus>("connecting");
	const [log, setLog] = useState<LogLine[]>([]);
	const [draft, setDraft] = useState("hello");
	const socket = useRef<EchoSocket | null>(null);
	const nextId = useRef(0);
	const inputId = useId();

	const append = (direction: LogLine["direction"], text: string) => {
		nextId.current += 1;
		const id = nextId.current;
		setLog((lines) => [...lines.slice(-49), { id, direction, text }]);
	};

	useEffect(() => {
		const s = new EchoSocket({
			url: sameOriginWsUrl(window.location, "/ws"),
			timer,
			createSocket,
			onStatus: setStatus,
			onMessage: (text) => append("received", text),
		});
		socket.current = s;
		s.connect();
		return () => {
			s.close();
			socket.current = null;
		};
	}, [timer, createSocket]);

	return (
		<div className='space-y-6'>
			<h1 className='font-heading text-2xl font-semibold'>WebSocket</h1>
			<Card>
				<CardHeader>
					<CardTitle className='flex items-center gap-3'>
						Echo <StatusBadge ok={status === "open"} label={status} />
					</CardTitle>
					<CardDescription>Messages go to /ws and come straight back. It reconnects with backoff if the server restarts.</CardDescription>
				</CardHeader>
				<CardContent className='space-y-4'>
					<form
						className='flex items-end gap-2'
						onSubmit={(e) => {
							e.preventDefault();
							if (draft !== "" && socket.current?.send(draft)) {
								append("sent", draft);
							}
						}}
					>
						<div className='flex-1 space-y-2'>
							<Label htmlFor={inputId}>Message</Label>
							<Input id={inputId} value={draft} onChange={(e) => setDraft(e.target.value)} />
						</div>
						<Button type='submit' disabled={status !== "open" || draft === ""}>
							Send
						</Button>
					</form>
					<ol aria-label='Messages' aria-live='polite' className='max-h-80 space-y-1 overflow-auto rounded-md border p-3 font-mono text-sm'>
						{log.length === 0 && <li className='text-muted-foreground'>Nothing yet.</li>}
						{log.map((line) => (
							<li key={line.id} data-direction={line.direction}>
								<span aria-hidden>{line.direction === "sent" ? "→ " : "← "}</span>
								<span className='sr-only'>{line.direction}: </span>
								{line.text}
							</li>
						))}
					</ol>
				</CardContent>
			</Card>
		</div>
	);
}

export const Route = createFileRoute("/websocket")({ component: WsPage });
