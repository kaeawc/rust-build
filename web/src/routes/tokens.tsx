import { useMutation } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useId, useState } from "react";
import { useServices } from "@/api/context";
import { ErrorAlert } from "@/components/error-alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { formatUnix } from "@/lib/format";

function TokensPage() {
	const { api } = useServices();
	const [sub, setSub] = useState("alice");
	const [ttl, setTtl] = useState("3600");
	const subId = useId();
	const ttlId = useId();
	const issue = useMutation({ mutationFn: () => api.issueToken({ sub, ttlSecs: Number(ttl) }) });
	const verify = useMutation({ mutationFn: (token: string) => api.me(token) });
	const token = issue.data?.token;
	return (
		<div className='space-y-6'>
			<h1 className='font-heading text-2xl font-semibold'>Tokens</h1>
			<div className='grid gap-4 md:grid-cols-2'>
				<Card>
					<CardHeader>
						<CardTitle>Issue a token</CardTitle>
						<CardDescription>POST /api/token signs an HS256 token.</CardDescription>
					</CardHeader>
					<CardContent>
						<form
							className='space-y-4'
							onSubmit={(e) => {
								e.preventDefault();
								verify.reset();
								issue.mutate();
							}}
						>
							<div className='space-y-2'>
								<Label htmlFor={subId}>Subject</Label>
								<Input id={subId} value={sub} onChange={(e) => setSub(e.target.value)} required />
							</div>
							<div className='space-y-2'>
								<Label htmlFor={ttlId}>Lifetime (seconds, max 86400)</Label>
								<Input id={ttlId} type='number' min={1} max={86400} value={ttl} onChange={(e) => setTtl(e.target.value)} required />
							</div>
							<Button type='submit' disabled={issue.isPending}>
								{issue.isPending ? "Issuing…" : "Issue token"}
							</Button>
							{issue.error && <ErrorAlert title='Could not issue a token' error={issue.error} />}
						</form>
					</CardContent>
				</Card>
				<Card>
					<CardHeader>
						<CardTitle>Verify it</CardTitle>
						<CardDescription>GET /api/me with the token as a bearer.</CardDescription>
					</CardHeader>
					<CardContent className='space-y-4'>
						{token ? (
							<>
								<div className='space-y-1'>
									<p className='text-sm text-muted-foreground'>
										Expires {issue.data ? formatUnix(issue.data.expiresAt) : ""}
									</p>
									<code data-testid='token' className='block max-h-32 overflow-auto rounded-md bg-muted p-2 font-mono text-xs break-all'>
										{token}
									</code>
								</div>
								<Button variant='outline' onClick={() => verify.mutate(token)} disabled={verify.isPending}>
									Call /api/me
								</Button>
							</>
						) : (
							<p className='text-sm text-muted-foreground'>Issue a token first.</p>
						)}
						{verify.data && (
							<pre data-testid='claims' className='overflow-auto rounded-md bg-muted p-2 font-mono text-xs'>
								{JSON.stringify(verify.data, null, 2)}
							</pre>
						)}
						{verify.error && <ErrorAlert title='Token rejected' error={verify.error} />}
					</CardContent>
				</Card>
			</div>
		</div>
	);
}

export const Route = createFileRoute("/tokens")({ component: TokensPage });
