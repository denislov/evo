#!/usr/bin/env node
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

const [inputPath, outputPath, configuredOverridesPath] = process.argv.slice(2);
if (!inputPath || !outputPath) {
  console.error(
    "usage: node crates/ai/tools/generate_models.cjs <models.generated.ts> <models_generated.json> [model_overrides.json]",
  );
  process.exit(2);
}

const overridesPath =
  configuredOverridesPath || path.join(__dirname, "model_overrides.json");
const overrides = JSON.parse(fs.readFileSync(overridesPath, "utf8"));
if (!Array.isArray(overrides)) {
  throw new Error("model overrides must be a JSON array");
}
const overridesByKey = new Map();
const overrideHits = new Map();
for (const override of overrides) {
  if (!override || typeof override.provider !== "string" || typeof override.id !== "string") {
    throw new Error("each model override must have string provider and id fields");
  }
  const key = `${override.provider}\u0000${override.id}`;
  if (overridesByKey.has(key)) {
    throw new Error(`duplicate model override ${override.provider}/${override.id}`);
  }
  overridesByKey.set(key, override);
  overrideHits.set(key, 0);
}

let source = fs.readFileSync(inputPath, "utf8");
source = source.replace(/^import type .*$/gm, "");
source = source.replace(/export const MODELS\s*=\s*/, "const MODELS = ");
source = source.replace(/\s+satisfies\s+Model<[^>]+>/g, "");
source = source.replace(/\s+as\s+const\s*;?\s*/g, ";\n");
source += "\nMODELS;";

const models = vm.runInNewContext(source, {}, { filename: inputPath });
const RETIRED_PROVIDERS = new Set(["amazon-bedrock"]);
const RETIRED_APIS = new Set(["bedrock-converse-stream"]);

function normalizeModel(m) {
  const model = {
    id: String(m.id),
    name: String(m.name),
    api: String(m.api),
    provider: String(m.provider),
    baseUrl: String(m.baseUrl),
    reasoning: Boolean(m.reasoning),
    input: m.input || ["text"],
    cost: {
      input: Number(m.cost?.input || 0),
      output: Number(m.cost?.output || 0),
      cacheRead: Number(m.cost?.cacheRead || 0),
      cacheWrite: Number(m.cost?.cacheWrite || 0),
    },
    contextWindow: Number(m.contextWindow || 0),
    maxTokens: Number(m.maxTokens || 0),
  };
  if (m.thinkingLevelMap !== undefined) {
    model.thinkingLevelMap = m.thinkingLevelMap;
  }
  if (m.headers !== undefined) {
    model.headers = m.headers;
  }
  if (m.compat !== undefined) {
    model.compat = m.compat;
  }
  return model;
}

function applyOverride(model) {
  const key = `${model.provider}\u0000${model.id}`;
  const override = overridesByKey.get(key);
  if (!override) return model;
  overrideHits.set(key, overrideHits.get(key) + 1);
  Object.assign(model, override.set || {});
  for (const field of override.remove || []) delete model[field];
  return model;
}

const out = [];
for (const provider of Object.keys(models).sort()) {
  for (const id of Object.keys(models[provider]).sort()) {
    const model = applyOverride(normalizeModel(models[provider][id]));
    if (RETIRED_PROVIDERS.has(model.provider) || RETIRED_APIS.has(model.api)) {
      continue;
    }
    out.push(model);
  }
}

for (const [key, hits] of overrideHits) {
  if (hits !== 1) {
    const [provider, id] = key.split("\u0000");
    throw new Error(
      `model override ${provider}/${id} matched ${hits} catalog entries; expected exactly one`,
    );
  }
}

fs.writeFileSync(outputPath, `${JSON.stringify(out, null, 2)}\n`);
