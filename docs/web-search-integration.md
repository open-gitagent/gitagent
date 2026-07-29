# You.com Web Search Tool

This document describes the You.com web search tool integration for GitAgent.

## Overview

The `web_search` tool allows GitAgent to search the web using You.com's Search API, providing access to current web information, research, news, and general queries.

## Features

- **Keyless operation**: Works without API key (100 free searches/day per IP)
- **Authenticated operation**: Enhanced features with `YDC_API_KEY`
- **Graceful error handling**: Clear error messages and fallback behavior
- **Configurable results**: Customizable result count (1-50 results)
- **Combined results**: Includes both web and news results
- **Proper attribution**: Includes source URLs, titles, and snippets

## Configuration

### Environment Variables

- `YDC_API_KEY` (optional): You.com API key for authenticated access
  - Without key: Uses keyless endpoint with 100 searches/day limit
  - With key: Higher quotas and enhanced features

### Agent Configuration

Add `web_search` to your agent's tools list in `agent.yaml`:

```yaml
tools:
  - cli
  - read
  - write
  - memory
  - web_search  # Add this line
```

## Usage

### CLI Usage

```bash
# Basic usage (will use web_search if configured)
gitagent --prompt "Search for latest TypeScript features"

# With specific tool selection
gitagent --prompt "Find information about AI agents" --allowed-tools web_search,read,write
```

### SDK Usage

```typescript
import { query } from 'gitagent';

for await (const msg of query({
  prompt: "Research recent developments in AI",
  tools: ['web_search']
})) {
  console.log(msg);
}
```

### Parameters

- `query` (string, required): Search query to find relevant web content
- `count` (number, optional): Number of results to return (default: 10, max: 50)

## API Endpoints

The tool automatically selects the appropriate endpoint:

- **Keyless**: `https://api.you.com/v1/agents/search` (no authentication)
- **Authenticated**: `https://ydc-index.io/v1/search` (with `X-API-Key` header)

## Error Handling

The tool provides informative error messages for common scenarios:

- **401 Unauthorized**: Invalid API key
- **429 Rate Limited**: Suggests API key upgrade for keyless users
- **5xx Server Errors**: Temporary service issues
- **Network Errors**: Connection problems
- **No Results**: Helpful suggestions for query refinement

## Output Format

Results are formatted as structured text with:

1. Result count and query confirmation
2. Numbered list of results with:
   - Title (bold)
   - URL
   - Snippet/description
3. API usage information (keyless vs authenticated)

Example output:
```
Found 3 results for "TypeScript AI frameworks":

1. **TypeScript AI Development Framework**
   URL: https://example.com/ts-ai
   A comprehensive framework for building AI applications with TypeScript...

2. **AI Agents in TypeScript**
   URL: https://example.com/ai-agents-ts
   Learn how to create intelligent agents using TypeScript and modern AI...

3. **TypeScript LLM Integration Guide** 
   URL: https://example.com/llm-ts
   Step-by-step guide for integrating large language models with TypeScript...

---
(Using keyless You.com Search API - set YDC_API_KEY for enhanced features)
```

## Security

- All web content is treated as untrusted external data
- No sensitive information is logged or exposed
- API keys are handled securely through environment variables
- Standard User-Agent header identifies the integration

## Integration Details

- **User-Agent**: `youdotcom-integration/open-gitagent-gitagent`
- **Tool Name**: `web_search`
- **Schema Validation**: Full TypeBox schema validation for parameters
- **Response Structure**: Structured with `content`, `isError`, and `details` fields