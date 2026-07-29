import type { AgentTool } from "@mariozechner/pi-agent-core";
import { Type } from "@sinclair/typebox";

// Schema for the web search tool
export const webSearchSchema = Type.Object({
	query: Type.String({ description: "Search query to find relevant web content" }),
	count: Type.Optional(Type.Number({ 
		description: "Number of search results to return (default: 10, max: 50)",
		minimum: 1,
		maximum: 50
	})),
});

export interface SearchResult {
	title: string;
	url: string;
	snippet: string;
}

export interface YouComSearchResponse {
	results?: {
		web?: SearchResult[];
		news?: SearchResult[];
	};
	error?: string;
}

/**
 * Create You.com web search tool.
 * Uses keyless API by default, falls back to authenticated API with YDC_API_KEY.
 */
export function createWebSearchTool(): AgentTool<typeof webSearchSchema> {
	return {
		name: "web_search",
		label: "web_search", 
		description: "Search the web using You.com for current information, research, news, and general queries. Returns titles, URLs, and snippets.",
		parameters: webSearchSchema,
		execute: async (
			_toolCallId: string,
			{ query, count = 10 }: { query: string; count?: number },
			signal?: AbortSignal,
		) => {
			try {
				const apiKey = process.env.YDC_API_KEY;
				const searchCount = Math.min(Math.max(count, 1), 50);

				// Choose endpoint based on API key availability
				const endpoint = apiKey 
					? "https://ydc-index.io/v1/search"
					: "https://api.you.com/v1/agents/search";

				// Build request options
				const requestOptions: RequestInit = {
					method: "GET",
					signal,
					headers: {
						"User-Agent": "youdotcom-integration/open-gitagent-gitagent",
						...(apiKey && { "X-API-Key": apiKey })
					},
				};

				// Build URL with query parameters
				const url = new URL(endpoint);
				url.searchParams.set("query", query);
				url.searchParams.set("count", searchCount.toString());

				const response = await fetch(url, requestOptions);

				if (!response.ok) {
					if (response.status === 401) {
						throw new Error(`Authentication failed. Please check your YDC_API_KEY environment variable.`);
					} else if (response.status === 429) {
						const upgradeMsg = apiKey 
							? "API rate limit exceeded. Consider upgrading your You.com plan."
							: "Rate limit exceeded on keyless API. Set YDC_API_KEY environment variable for higher limits.";
						throw new Error(upgradeMsg);
					} else if (response.status >= 500) {
						throw new Error(`You.com service temporarily unavailable (${response.status}). Please try again later.`);
					} else {
						const errorText = await response.text().catch(() => "");
						throw new Error(`You.com API error (${response.status}): ${errorText || "Unknown error"}`);
					}
				}

				const data: YouComSearchResponse = await response.json();

				if (data.error) {
					throw new Error(`You.com API error: ${data.error}`);
				}

				// Combine web and news results
				const webResults = data.results?.web || [];
				const newsResults = data.results?.news || [];
				const allResults = [...webResults, ...newsResults];

				if (allResults.length === 0) {
					return {
						content: [{
							type: "text",
							text: `No results found for "${query}". Try a different search query or check your spelling.`
						}],
						isError: false,
						details: { resultCount: 0, query }
					};
				}

				// Format results
				let resultText = `Found ${allResults.length} results for "${query}":\n\n`;

				allResults.slice(0, searchCount).forEach((result, index) => {
					resultText += `${index + 1}. **${result.title}**\n`;
					resultText += `   URL: ${result.url}\n`;
					resultText += `   ${result.snippet}\n\n`;
				});

				// Add API usage info
				const apiInfo = apiKey 
					? "(Using authenticated You.com Search API)"
					: "(Using keyless You.com Search API - set YDC_API_KEY for enhanced features)";
				
				resultText += `\n---\n${apiInfo}`;

				return {
					content: [{
						type: "text", 
						text: resultText
					}],
					isError: false,
					details: { 
						resultCount: allResults.length, 
						query, 
						apiKeyUsed: !!apiKey,
						endpoint: apiKey ? "ydc-index.io" : "api.you.com"
					}
				};

			} catch (error) {
				// Handle network and other errors
				if (error instanceof Error) {
					if (error.name === "AbortError") {
						throw new Error("Search operation was cancelled.");
					}
					
					if (error.message.includes("fetch")) {
						throw new Error("Network error: Unable to reach You.com API. Please check your internet connection.");
					}

					// Re-throw known API errors
					throw error;
				}
				
				throw new Error(`Unexpected error during web search: ${String(error)}`);
			}
		},
	};
}