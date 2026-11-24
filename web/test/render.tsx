import { createMemoryHistory } from "@tanstack/react-router";
import { render } from "@testing-library/react";
import { App } from "@/app";
import { createQueryClient } from "@/router";
import { FakeApiClient } from "./fakes/api";
import { FakeSocket } from "./fakes/socket";
import { FakeTimer } from "./fakes/timer";

/** Renders the whole app at `path` with fakes for every service. */
export function renderApp(path: string, api = new FakeApiClient()) {
	const timer = new FakeTimer();
	const queryClient = createQueryClient();
	queryClient.setDefaultOptions({ queries: { retry: false, refetchInterval: false } });
	const history = createMemoryHistory({ initialEntries: [path] });
	const view = render(<App services={{ api, timer, createSocket: FakeSocket.create }} queryClient={queryClient} history={history} />);
	return { ...view, api, timer, history };
}
