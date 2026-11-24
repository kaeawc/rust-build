import type { QueryClient } from "@tanstack/react-query";
import { Link, Outlet, createRootRouteWithContext } from "@tanstack/react-router";
import { Activity, KeyRound, LayoutDashboard, MessagesSquare, Users } from "lucide-react";

export interface RouterContext {
	queryClient: QueryClient;
}

const nav = [
	{ to: "/", label: "Dashboard", icon: LayoutDashboard },
	{ to: "/users", label: "Users", icon: Users },
	{ to: "/tokens", label: "Tokens", icon: KeyRound },
	{ to: "/admin", label: "Admin", icon: Activity },
	// Not `/ws`: that path is the server's WebSocket endpoint.
	{ to: "/websocket", label: "WebSocket", icon: MessagesSquare },
] as const;

function Shell() {
	return (
		<div className='min-h-svh bg-background text-foreground'>
			<header className='border-b'>
				<div className='mx-auto flex max-w-5xl flex-wrap items-center gap-x-6 gap-y-2 px-4 py-3'>
					<Link to='/' className='font-heading text-lg font-semibold tracking-tight'>
						rust-build
					</Link>
					<nav aria-label='Main' className='flex flex-wrap gap-1'>
						{nav.map(({ to, label, icon: Icon }) => (
							<Link
								key={to}
								to={to}
								activeOptions={{ exact: to === "/" }}
								className='inline-flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-sm'
								activeProps={{ className: "bg-muted font-medium text-foreground" }}
								inactiveProps={{ className: "text-muted-foreground hover:bg-muted hover:text-foreground" }}
							>
								<Icon className='size-4' aria-hidden />
								{label}
							</Link>
						))}
					</nav>
				</div>
			</header>
			<main className='mx-auto max-w-5xl px-4 py-6'>
				<Outlet />
			</main>
		</div>
	);
}

function NotFound() {
	return (
		<div className='space-y-2'>
			<h1 className='font-heading text-2xl font-semibold'>Not found</h1>
			<p className='text-muted-foreground'>No page lives at this address.</p>
			<Link to='/' className='text-primary underline-offset-4 hover:underline'>
				Back to the dashboard
			</Link>
		</div>
	);
}

export const Route = createRootRouteWithContext<RouterContext>()({
	component: Shell,
	notFoundComponent: NotFound,
});
