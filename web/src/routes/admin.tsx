import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useServices } from "@/api/context";
import { queries } from "@/api/queries";
import { ErrorAlert } from "@/components/error-alert";
import { StatusBadge } from "@/components/status-badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { formatMs } from "@/lib/format";

function AdminPage() {
	const { api } = useServices();
	const traffic = useQuery(queries.traffic(api));
	const checks = useQuery(queries.healthchecks(api));
	return (
		<div className='space-y-6'>
			<h1 className='font-heading text-2xl font-semibold'>Admin</h1>
			<Card>
				<CardHeader>
					<CardTitle>Traffic</CardTitle>
					<CardDescription>Requests per matched API route, from GET /api/admin/traffic.</CardDescription>
				</CardHeader>
				<CardContent>
					{traffic.error ? (
						<ErrorAlert title='Could not load traffic' error={traffic.error} />
					) : (
						<Table>
							<TableHeader>
								<TableRow>
									<TableHead>Route</TableHead>
									<TableHead className='text-right'>Requests</TableHead>
									<TableHead className='text-right'>2xx</TableHead>
									<TableHead className='text-right'>4xx</TableHead>
									<TableHead className='text-right'>5xx</TableHead>
									<TableHead className='text-right'>Last</TableHead>
									<TableHead className='text-right'>Mean latency</TableHead>
								</TableRow>
							</TableHeader>
							<TableBody>
								{traffic.data?.map((t) => (
									<TableRow key={`${t.method} ${t.path}`}>
										<TableCell className='font-mono'>
											{t.method} {t.path}
										</TableCell>
										<TableCell className='text-right tabular-nums'>{t.count}</TableCell>
										<TableCell className='text-right tabular-nums'>{t.status2xx}</TableCell>
										<TableCell className='text-right tabular-nums'>{t.status4xx}</TableCell>
										<TableCell className='text-right tabular-nums'>{t.status5xx}</TableCell>
										<TableCell className='text-right tabular-nums'>{t.lastStatus}</TableCell>
										<TableCell className='text-right tabular-nums'>{formatMs(t.count > 0 ? t.totalNanos / t.count : 0)}</TableCell>
									</TableRow>
								))}
								{traffic.data?.length === 0 && (
									<TableRow>
										<TableCell colSpan={7} className='text-center text-muted-foreground'>
											No API requests yet.
										</TableCell>
									</TableRow>
								)}
							</TableBody>
						</Table>
					)}
				</CardContent>
			</Card>
			<Card>
				<CardHeader>
					<CardTitle>Health checks</CardTitle>
					<CardDescription>Every readiness probe, from GET /api/admin/healthchecks.</CardDescription>
				</CardHeader>
				<CardContent>
					{checks.error ? (
						<ErrorAlert title='Could not run health checks' error={checks.error} />
					) : (
						<Table>
							<TableHeader>
								<TableRow>
									<TableHead>Probe</TableHead>
									<TableHead>Status</TableHead>
									<TableHead className='text-right'>Latency</TableHead>
								</TableRow>
							</TableHeader>
							<TableBody>
								{checks.data?.map((c) => (
									<TableRow key={c.name}>
										<TableCell>{c.name}</TableCell>
										<TableCell>
											<StatusBadge ok={c.healthy} label={c.healthy ? "healthy" : (c.err ?? "failing")} />
										</TableCell>
										<TableCell className='text-right tabular-nums'>{formatMs(c.latencyNs)}</TableCell>
									</TableRow>
								))}
							</TableBody>
						</Table>
					)}
				</CardContent>
			</Card>
		</div>
	);
}

export const Route = createFileRoute("/admin")({ component: AdminPage });
