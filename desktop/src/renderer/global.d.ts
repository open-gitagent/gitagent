import type { GitagentApi } from "../shared/types";

declare global {
	interface Window {
		gitagent: GitagentApi;
	}
}

export {};
