// USD per million tokens for Claude models
const MODEL_PRICING: Record<string, { input: number; output: number; cacheRead: number; cacheWrite: number }> = {
  'opus':   { input: 15.00, output: 75.00, cacheRead: 1.50, cacheWrite: 18.75 },
  'sonnet': { input: 3.00,  output: 15.00, cacheRead: 0.30, cacheWrite: 3.75  },
  'haiku':  { input: 0.80,  output: 4.00,  cacheRead: 0.08, cacheWrite: 1.00  },
}

export function computeCost(
  model: string | null,
  input: number,
  output: number,
  cacheRead: number,
  cacheWrite: number,
): number {
  if (!model) return 0
  const key = Object.keys(MODEL_PRICING).find(k => model.includes(k))
  if (!key) return 0
  const p = MODEL_PRICING[key]
  return (input * p.input + output * p.output + cacheRead * p.cacheRead + cacheWrite * p.cacheWrite) / 1_000_000
}
