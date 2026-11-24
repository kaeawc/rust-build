import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import type { ProbeResponse } from "@/api/client";
import { useServices } from "@/api/context";
import { queries } from "@/api/queries";
import { ErrorAlert } from "@/components/error-alert";
import { StatusBadge } from "@/components/status-badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { formatMs, formatUptime } from "@/lib/format";

function Fact({ term, children }: { term: string; children: React.ReactNode }) {
	return (
		<div className='flex justify-between gap-4 py-1'>
			<dt className='text-muted-foreground'>{term}</dt>
			<dd className='font-mono text-sm'>{children}</dd>
		</div>
	);
}

function ProbeCard({ title, path, probe }: { title: string; path: string; probe: { data: ProbeResponse | undefined; error: unknown } }) {
	const body = probe.data?.body;
	return (
		<Card>
			<CardHeader>
				<CardTitle>{title}</CardTitle>
				<CardDescription className='font-mono'>{path}</CardDescription>
			</CardHeader>
			<CardContent className='space-y-3'>
				{probe.error ? (
					<ErrorAlert title={`${path} failed`} error={probe.error} />
				) : body ? (
					<>
						<StatusBadge ok={body.status === "ok"} label={body.status} />
						{body.probes && body.probes.length > 0 && (
							<ul className='space-y-1 text-sm'>
								{body.probes.map((p) => (
									<li key={p.name} className='flex items-center justify-between gap-2'>
										<span>{p.name}</span>
										<span className='flex items-center gap-2'>
											<span className='font-mono text-muted-foreground'>{formatMs(p.latencyNs)}</span>
											<StatusBadge ok={p.healthy} label={p.healthy ? "healthy" : (p.err ?? "failing")} />
										</span>
									</li>
								))}
							</ul>
						)}
					</>
				) : (
					<Skeleton className='h-5 w-16' />
				)}
			</CardContent>
		</Card>
	);
}

function Dashboard() {
	const { api } = useServices();
	const info = useQuery(queries.info(api));
	const health = useQuery(queries.health(api));
	const ready = useQuery(queries.ready(api));
	return (
		<div className='space-y-6'>
			<h1 className='font-heading text-2xl font-semibold'>Dashboard</h1>
			<div className='grid gap-4 md:grid-cols-3'>
				<Card>
					<CardHeader>
						<CardTitle>Server</CardTitle>
						<CardDescription className='font-mono'>/api/info</CardDescription>
					</CardHeader>
					<CardContent>
						{info.error ? (
							<ErrorAlert title='Server info unavailable' error={info.error} />
						) : info.data ? (
							<dl>
								<Fact term='Version'>{info.data.version}</Fact>
								<Fact term='Protocol'>{info.data.httpVersion}</Fact>
								<Fact term='Uptime'>{formatUptime(info.data.uptimeSecs)}</Fact>
								<Fact term='Your address'>{info.data.peer ?? "unknown"}</Fact>
							</dl>
						) : (
							<Skeleton className='h-20 w-full' />
						)}
					</CardContent>
				</Card>
				<ProbeCard title='Liveness' path='/healthz' probe={health} />
				<ProbeCard title='Readiness' path='/readyz' probe={ready} />
			</div>
		</div>
	);
}

export const Route = createFileRoute("/")({ component: Dashboard });
