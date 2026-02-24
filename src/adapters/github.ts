import { resolve, join } from 'node:path';
import { loadAgentManifest } from '../utils/loader.js';
import { exportToSystemPrompt } from './system-prompt.js';

export interface GitHubModelsPayload {
  model: string;
  messages: Array<{ role: string; content: string }>;
  temperature: number;
  max_tokens: number;
  stream: boolean;
}

/**
 * Map an agent.yaml model to a GitHub Models model ID (vendor/model).
 */
function resolveModel(model?: string): string {
  if (!model) return 'openai/gpt-4.1';
  if (model.includes('/')) return model;

  if (model.startsWith('gpt') || model.startsWith('o1') || model.startsWith('o3') || model.startsWith('o4')) {
    return `openai/${model}`;
  }
  if (model.startsWith('claude')) return `anthropic/${model}`;
  if (model.startsWith('llama') || model.startsWith('Llama')) return `meta/${model}`;
  if (model.startsWith('mistral') || model.startsWith('Mistral')) return `mistralai/${model}`;
  if (model.startsWith('gemini')) return `google/${model}`;
  if (model.startsWith('deepseek') || model.startsWith('DeepSeek')) return `deepseek/${model}`;

  return model;
}

/**
 * Export a gitagent directory to a GitHub Models API-ready payload.
 */
export function exportToGitHub(dir: string): GitHubModelsPayload {
  const agentDir = resolve(dir);
  const manifest = loadAgentManifest(agentDir);
  const systemPrompt = exportToSystemPrompt(agentDir);

  return {
    model: resolveModel(manifest.model?.preferred),
    messages: [
      { role: 'system', content: systemPrompt },
    ],
    temperature: manifest.model?.constraints?.temperature ?? 0.3,
    max_tokens: manifest.model?.constraints?.max_tokens ?? 4096,
    stream: true,
  };
}

/**
 * String export for `gitagent export --format github`.
 */
export function exportToGitHubString(dir: string): string {
  const payload = exportToGitHub(dir);
  return JSON.stringify(payload, null, 2);
}
