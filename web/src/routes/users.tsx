import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, createFileRoute } from "@tanstack/react-router";
import { Plus } from "lucide-react";
import { useId, useState } from "react";
import type { UsersQuery } from "@/api/client";
import { useServices } from "@/api/context";
import { queries } from "@/api/queries";
import { ErrorAlert } from "@/components/error-alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
	DialogTrigger,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";

export const PAGE_SIZES = [10, 20, 50] as const;

/** `?pageSize=&cursor=`, keeping only valid values. */
export function parseUsersSearch(search: Record<string, unknown>): UsersQuery {
	const out: UsersQuery = {};
	const size = Number(search["pageSize"]);
	if (Number.isInteger(size) && size >= 1 && size <= 100) {
		out.pageSize = size;
	}
	const cursor = search["cursor"];
	if (typeof cursor === "string" && cursor.length > 0) {
		out.cursor = cursor;
	}
	return out;
}

function NewUserDialog() {
	const { api } = useServices();
	const queryClient = useQueryClient();
	const [open, setOpen] = useState(false);
	const [name, setName] = useState("");
	const inputId = useId();
	const create = useMutation({
		mutationFn: (newName: string) => api.createUser({ name: newName }),
		onSuccess: async () => {
			setOpen(false);
			setName("");
			await queryClient.invalidateQueries({ queryKey: ["users"] });
		},
	});
	return (
		<Dialog
			open={open}
			onOpenChange={(next) => {
				setOpen(next);
				if (!next) {
					create.reset();
				}
			}}
		>
			<DialogTrigger asChild>
				<Button>
					<Plus data-icon='inline-start' aria-hidden />
					New user
				</Button>
			</DialogTrigger>
			<DialogContent>
				<form
					className='space-y-4'
					onSubmit={(e) => {
						e.preventDefault();
						create.mutate(name);
					}}
				>
					<DialogHeader>
						<DialogTitle>New user</DialogTitle>
						<DialogDescription>Creates a user with POST /api/users.</DialogDescription>
					</DialogHeader>
					<div className='space-y-2'>
						<Label htmlFor={inputId}>Name</Label>
						<Input id={inputId} value={name} onChange={(e) => setName(e.target.value)} maxLength={100} autoComplete='off' required />
					</div>
					{create.error && <ErrorAlert title='Could not create the user' error={create.error} />}
					<DialogFooter>
						<Button type='submit' disabled={create.isPending || name.trim() === ""}>
							{create.isPending ? "Creating…" : "Create"}
						</Button>
					</DialogFooter>
				</form>
			</DialogContent>
		</Dialog>
	);
}

function UsersPage() {
	const { api } = useServices();
	const search = Route.useSearch();
	const users = useQuery(queries.users(api, search));
	const pageSize = search.pageSize ?? 20;
	return (
		<div className='space-y-6'>
			<div className='flex items-center justify-between gap-4'>
				<h1 className='font-heading text-2xl font-semibold'>Users</h1>
				<NewUserDialog />
			</div>
			<Card>
				<CardHeader>
					<CardTitle>All users</CardTitle>
					<CardDescription>Cursor-paginated from GET /api/users ({pageSize} per page).</CardDescription>
				</CardHeader>
				<CardContent className='space-y-4'>
					{users.error ? (
						<ErrorAlert title='Could not load users' error={users.error} />
					) : (
						<Table>
							<TableHeader>
								<TableRow>
									<TableHead className='w-24'>ID</TableHead>
									<TableHead>Name</TableHead>
								</TableRow>
							</TableHeader>
							<TableBody>
								{users.data?.items.map((u) => (
									<TableRow key={u.id}>
										<TableCell className='font-mono'>{u.id}</TableCell>
										<TableCell>{u.name}</TableCell>
									</TableRow>
								))}
								{users.data?.items.length === 0 && (
									<TableRow>
										<TableCell colSpan={2} className='text-center text-muted-foreground'>
											No users yet.
										</TableCell>
									</TableRow>
								)}
								{users.isPending && (
									<TableRow>
										<TableCell colSpan={2} className='text-center text-muted-foreground'>
											Loading…
										</TableCell>
									</TableRow>
								)}
							</TableBody>
						</Table>
					)}
					<nav aria-label='Pagination' className='flex items-center justify-between gap-2'>
						<div className='flex gap-1 text-sm'>
							<span className='text-muted-foreground'>Per page:</span>
							{PAGE_SIZES.map((size) => (
								<Link
									key={size}
									to='/users'
									search={{ pageSize: size }}
									className='rounded px-1.5 underline-offset-4 hover:underline'
									activeProps={{ className: "font-semibold" }}
									activeOptions={{ includeSearch: true }}
								>
									{size}
								</Link>
							))}
						</div>
						<div className='flex gap-2'>
							{search.cursor && (
								<Button variant='outline' asChild>
									<Link to='/users' search={{ pageSize }}>
										First page
									</Link>
								</Button>
							)}
							{users.data?.hasMore && users.data.nextCursor && (
								<Button variant='outline' asChild>
									<Link to='/users' search={{ pageSize, cursor: users.data.nextCursor }}>
										Next page
									</Link>
								</Button>
							)}
						</div>
					</nav>
				</CardContent>
			</Card>
		</div>
	);
}

export const Route = createFileRoute("/users")({
	validateSearch: parseUsersSearch,
	component: UsersPage,
});
