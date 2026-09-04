#!/usr/bin/env node
/**
 * Phase 0 profile-fixture and compatibility-inventory generator (issue 1).
 *
 * Operator-run, not a CI or release gate. Resolves the sibling LLxprt Code
 * checkout's lockfile-pinned TypeScript compiler API (bun-cache path from the
 * sibling's own bun.lock / package.json) and uses the compiler AST to derive the
 * persistable profile-key inventory, alias rules, duplicates, and application
 * paths. With --profiles it also captures the structure of the installed
 * profiles directory and writes deterministic, value-free, redacted fixtures.
 *
 * Security contract (issue 1): this generator never prints, returns, persists,
 * or exposes scalar credential values, account ids, tokens, local paths, or
 * credential-store data. Emitted values are key paths and JSON types only, plus
 * frozen safe enums and the fixed redacted placeholders. A persistToProfile
 * entry whose key comes from an unsupported computed expression fails generation
 * rather than being skipped or guessed.
 *
 * The checks in tests/profile_compatibility.rs (repository self-tests) verify
 * the checked-in artifact for internal consistency and require every tracked
 * fixture field to have exactly one classification and one owner.
 *
 * Usage:
 *   node scripts/generate-profile-compatibility-inventory.mjs \
 *       --sibling <absolute-llxprt-code-checkout> \
 *       --profiles <absolute-profile-directory>
 *   node scripts/generate-profile-compatibility-inventory.mjs --self-test
 */

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

const RUST_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const FIXTURES_ROOT = path.join(RUST_ROOT, 'tests', 'fixtures', 'profiles');
const ARTIFACT_PATH = path.join(
  RUST_ROOT,
  'tests',
  'fixtures',
  'profile-compatibility-inventory.json',
);
const DOC_PATH = path.join(RUST_ROOT, 'docs', 'profile-compatibility.md');

const UNSUPPORTED_COMPUTED_DIAGNOSTIC =
  'unsupported computed expression in persistToProfile entry: unsupported computed form';

// Frozen, non-credential redaction placeholders.
const PLACEHOLDER_CREDENTIAL = '<redacted-credential>';
const PLACEHOLDER_KEYFILE = '<redacted-keyfile-path>';
const PLACEHOLDER_PATH = '<redacted-local-path>';

// Exact credential-source spellings whose installed scalar becomes a placeholder.
const CREDENTIAL_SOURCE_PLACEHOLDERS = new Map([
  ['auth-key', PLACEHOLDER_CREDENTIAL],
  ['authKey', PLACEHOLDER_CREDENTIAL],
  ['api-key', PLACEHOLDER_CREDENTIAL],
  ['apiKey', PLACEHOLDER_CREDENTIAL],
  ['auth-key-name', PLACEHOLDER_CREDENTIAL],
  ['auth-keyfile', PLACEHOLDER_KEYFILE],
  ['authKeyfile', PLACEHOLDER_KEYFILE],
  ['api-keyfile', PLACEHOLDER_KEYFILE],
  ['apiKeyfile', PLACEHOLDER_KEYFILE],
]);

const SECRET_LIKE_LAST_SEGMENT = /\b(auth|key|token|secret|credential|password)\b/i;

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, 'utf8'));
}

// ---------------------------------------------------------------------------
// Sibling lockfile-pinned TypeScript resolution.
// ---------------------------------------------------------------------------
function resolveLockfilePinnedTypeScript(siblingRoot) {
  const bunLock = path.join(siblingRoot, 'bun.lock');
  const packageJson = path.join(siblingRoot, 'package.json');
  let pinned = null;
  let source = null;
  if (fs.existsSync(packageJson)) {
    const pkg = readJson(packageJson);
    if (typeof pkg.dependencies?.typescript === 'string') {
      pinned = pkg.dependencies.typescript;
      source = 'package.json dependencies';
    } else if (typeof pkg.devDependencies?.typescript === 'string') {
      pinned = pkg.devDependencies.typescript;
      source = 'package.json devDependencies';
    }
  }
  if (typeof pinned !== 'string' || pinned.length === 0) {
    if (fs.existsSync(bunLock)) {
      const text = fs.readFileSync(bunLock, 'utf8');
      const m = text.match(/^\s*"typescript":\s*\[\s*"typescript@([^"@]+)"/m);
      if (m) {
        pinned = m[1];
        source = 'bun.lock';
      }
    }
  }
  if (typeof pinned !== 'string' || pinned.length === 0) {
    return {
      status: 'unpinned',
      version: null,
      entry: null,
      error:
        'sibling checkout pins no typescript version (package.json dependencies/devDependencies or bun.lock)',
    };
  }
  const version = pinned.replace(/^[\^~=]/, '');
  const candidates = [
    path.join(
      os.homedir(),
      '.bun',
      'install',
      'cache',
      `typescript@${version}@@@1`,
      'lib',
      'typescript.js',
    ),
    path.join(siblingRoot, 'node_modules', 'typescript', 'lib', 'typescript.js'),
  ];
  for (const entry of candidates) {
    if (fs.existsSync(entry)) {
      return { status: 'resolved', version, source, entry };
    }
  }
  return {
    status: 'unresolved',
    version,
    source,
    entry: null,
    error: `typescript ${version} not found in bun cache or sibling node_modules (${source})`,
  };
}

function loadTypeScript(siblingRoot) {
  const resolved = resolveLockfilePinnedTypeScript(siblingRoot);
  if (resolved.status !== 'resolved') {
    throw new Error(resolved.error);
  }
  const require = createRequire(path.join(siblingRoot, 'package.json'));
  const ts = require(resolved.entry);
  if (typeof ts.version !== 'string') {
    throw new Error(`lockfile-pinned typescript entry exposes no version: ${resolved.entry}`);
  }
  if (ts.version !== resolved.version) {
    throw new Error(
      `lockfile-pinned typescript version mismatch: expected ${resolved.version}, loaded ${ts.version}`,
    );
  }
  return { ts, version: ts.version, entry: resolved.entry };
}

function siblingSources(siblingRoot) {
  const rels = [
    'packages/settings/src/settings/registry/registry-entries-1.ts',
    'packages/settings/src/settings/registry/registry-entries-2.ts',
    'packages/settings/src/settings/registry/registry-entries-3.ts',
    'packages/settings/src/settings/registry/registry-types.ts',
    'packages/settings/src/settings/settingsRegistry.ts',
    'packages/settings/src/profiles/ProfileManager.ts',
    'packages/providers/src/runtime/profileSnapshot.ts',
    'packages/providers/src/runtime/profileApplication.ts',
  ];
  return rels.map((rel) => ({ rel, abs: path.join(siblingRoot, rel) }));
}

function readSource(ts, abs) {
  const text = fs.readFileSync(abs, 'utf8');
  return ts.createSourceFile(abs, text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
}

function literalText(node, ts) {
  if (!node) return undefined;
  if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) return node.text;
  if (node.kind === ts.SyntaxKind.TrueKeyword) return true;
  if (node.kind === ts.SyntaxKind.FalseKeyword) return false;
  if (ts.isNumericLiteral(node) && node.text !== '') return Number(node.text);
  return undefined;
}

function collectConstArrays(ts, sourceFile) {
  const out = new Map();
  for (const st of sourceFile.statements) {
    if (!ts.isVariableStatement(st)) continue;
    for (const decl of st.declarationList.declarations) {
      if (!ts.isIdentifier(decl.name) || !ts.isArrayLiteralExpression(decl.initializer)) continue;
      const values = [];
      let ok = true;
      for (const el of decl.initializer.elements) {
        const t = literalText(el, ts);
        if (typeof t !== 'string') {
          ok = false;
          break;
        }
        values.push(t);
      }
      if (ok) out.set(decl.name.text, values);
    }
  }
  return out;
}

function importedConstArrays(ts, sourceFile, typeConsts) {
  const out = new Map();
  for (const st of sourceFile.statements) {
    if (!ts.isImportDeclaration(st)) continue;
    const mod = ts.isStringLiteral(st.moduleSpecifier) ? st.moduleSpecifier.text : '';
    if (mod !== './registry-types.js' && mod !== './registry-types') continue;
    const clause = st.importClause;
    if (!clause || !clause.namedBindings || clause.namedBindings.kind !== ts.SyntaxKind.NamedImports) {
      continue;
    }
    for (const el of clause.namedBindings.elements) {
      if (ts.isIdentifier(el.name) && typeConsts.has(el.name.text)) {
        out.set(el.name.text, typeConsts.get(el.name.text));
      }
    }
  }
  return out;
}

/**
 * Expand a SpreadElement into specs. Supported (only these):
 *   ...(<literal array of object literals>).map(({ key, ... }) => ({ key, ... }))
 *   ...[importedConstStringLiteralArray]
 * Every other computed expression fails generation.
 */
function expandSpread(expression, ts, sourceFile, fail) {
  if (ts.isIdentifier(expression)) {
    fail(`${UNSUPPORTED_COMPUTED_DIAGNOSTIC} (${sourceFile.fileName})`);
    return { kind: 'none' };
  }
  if (
    ts.isCallExpression(expression) &&
    expression.expression.kind === ts.SyntaxKind.PropertyAccessExpression &&
    ts.isIdentifier(expression.expression.name) &&
    expression.expression.name.text === 'map'
  ) {
    const arrayExpr = expression.expression.expression;
    const arrow = expression.arguments[0];
    if (!ts.isArrayLiteralExpression(arrayExpr) || !arrow || !ts.isArrowFunction(arrow)) {
      fail(
        `unsupported computed form: .map() spread must map a literal array with an arrow function (${sourceFile.fileName})`,
      );
      return { kind: 'none' };
    }
    const param = arrow.parameters[0];
    const bindings = new Map();
    if (param && ts.isObjectBindingPattern(param.name)) {
      for (const el of param.name.elements) {
        if (ts.isBindingElement(el) && ts.isIdentifier(el.name)) {
          // A shorthand `{ key }` binding has no propertyName; the property
          // spelling is the binding name itself.
          const prop = el.propertyName && ts.isIdentifier(el.propertyName) ? el.propertyName.text : el.name.text;
          bindings.set(el.name.text, prop);
        }
      }
    }
    const body = arrow.body;
    // The arrow body is often parenthesized: `({ key, ... })`.
    let returned = body;
    while (returned && ts.isParenthesizedExpression(returned)) returned = returned.expression;
    if (!ts.isObjectLiteralExpression(returned)) {
      fail(`unsupported computed form: .map() arrow must return an object literal (${sourceFile.fileName})`);
      return { kind: 'none' };
    }
    const specs = [];
    for (const element of arrayExpr.elements) {
      if (!ts.isObjectLiteralExpression(element)) {
        fail(
          `unsupported computed form: .map() source must be a literal array of object literals (${sourceFile.fileName})`,
        );
        return { kind: 'none' };
      }
      const srcProps = {};
      for (const p of element.properties) {
        if (ts.isPropertyAssignment(p) && ts.isIdentifier(p.name) && p.initializer) {
          srcProps[p.name.text] = literalText(p.initializer, ts);
        }
      }
      const spec = {};
      for (const p of returned.properties) {
        if (ts.isShorthandPropertyAssignment(p) && ts.isIdentifier(p.name)) {
          const bound = bindings.get(p.name.text);
          if (bound === undefined) {
            fail(
              `unsupported computed form: shorthand ${p.name.text} is not bound from a map element (${sourceFile.fileName})`,
            );
            return { kind: 'none' };
          }
          spec[p.name.text] = srcProps[bound];
          continue;
        }
        if (ts.isPropertyAssignment(p) && ts.isIdentifier(p.name) && p.initializer) {
          if (p.name.text === 'key' || p.name.text === 'type' || p.name.text === 'persistToProfile') {
            spec[p.name.text] = literalText(p.initializer, ts);
          }
        }
      }
      if (typeof spec.key !== 'string') {
        fail(`unsupported computed form: mapped spec loses a literal key (${sourceFile.fileName})`);
        return { kind: 'none' };
      }
      specs.push(spec);
    }
    return { kind: 'specs', value: specs };
  }
  fail(`${UNSUPPORTED_COMPUTED_DIAGNOSTIC} (${sourceFile.fileName})`);
  return { kind: 'none' };
}

function deriveEntries(ts, typeSource, registryFiles) {
  const errors = [];
  const fail = (msg) => errors.push(msg);
  const entries = [];
  const typeConsts = collectConstArrays(ts, typeSource);
  const registryRel = [
    'packages/settings/src/settings/registry/registry-entries-1.ts',
    'packages/settings/src/settings/registry/registry-entries-2.ts',
    'packages/settings/src/settings/registry/registry-entries-3.ts',
  ];
  for (const rel of registryRel) {
    const found = registryFiles.find((s) => s.rel === rel);
    if (!found) {
      fail(`missing sibling registry source: ${rel}`);
      continue;
    }
    const sf = readSource(ts, found.abs);
    const importedConsts = importedConstArrays(ts, sf, typeConsts);
    for (const st of sf.statements) {
      if (!ts.isVariableStatement(st)) continue;
      for (const decl of st.declarationList.declarations) {
        if (!ts.isIdentifier(decl.name) || !ts.isArrayLiteralExpression(decl.initializer)) continue;
        for (const element of decl.initializer.elements) {
          if (ts.isSpreadElement(element)) {
            if (ts.isIdentifier(element.expression) && importedConsts.has(element.expression.text)) {
              for (const v of importedConsts.get(element.expression.text)) {
                entries.push({ key: v, type: null, file: rel, order: entries.length, produced: 'literal' });
              }
              continue;
            }
            const expanded = expandSpread(element.expression, ts, sf, fail);
            if (expanded.kind === 'specs') {
              for (const s of expanded.value) {
                entries.push({
                  key: s.key,
                  type: typeof s.type === 'string' ? s.type : null,
                  file: rel,
                  order: entries.length,
                  produced: 'literal',
                });
              }
            }
            continue;
          }
          if (ts.isObjectLiteralExpression(element)) {
            let key = undefined;
            let type = null;
            let persist = false;
            let seenKey = false;
            for (const p of element.properties) {
              if (!ts.isPropertyAssignment(p) || !ts.isIdentifier(p.name) || !p.initializer) continue;
              if (p.name.text === 'key') {
                const t = literalText(p.initializer, ts);
                if (typeof t === 'string') {
                  key = t;
                  seenKey = true;
                }
              } else if (p.name.text === 'type') {
                const t = literalText(p.initializer, ts);
                if (typeof t === 'string') type = t;
              } else if (p.name.text === 'persistToProfile') {
                persist = literalText(p.initializer, ts) === true;
              }
            }
            if (persist) {
              if (!seenKey) {
                fail(`unsupported computed form: persistToProfile entry without a literal key (${rel})`);
                continue;
              }
              entries.push({ key, type, file: rel, order: entries.length, produced: 'literal' });
            }
          }
        }
      }
    }
  }
  return { entries, errors };
}

function deriveAliasesAndRules(ts, sources) {
  const aliases = new Map();
  const errors = [];
  const rels = [
    'settings/registry/registry-entries-1.ts',
    'settings/registry/registry-entries-2.ts',
    'settings/registry/registry-entries-3.ts',
  ];
  for (const rel of rels) {
    const found = sources.find((s) => s.rel === rel);
    if (!found) continue;
    const sf = readSource(ts, found.abs);
    for (const st of sf.statements) {
      if (!ts.isVariableStatement(st)) continue;
      for (const decl of st.declarationList.declarations) {
        if (!ts.isIdentifier(decl.name) || !ts.isArrayLiteralExpression(decl.initializer)) continue;
        for (const element of decl.initializer.elements) {
          if (!ts.isObjectLiteralExpression(element)) continue;
          let key = undefined;
          const aliasesArr = [];
          for (const p of element.properties) {
            if (ts.isPropertyAssignment(p) && ts.isIdentifier(p.name) && p.initializer) {
              if (p.name.text === 'key') {
                const t = literalText(p.initializer, ts);
                if (typeof t === 'string') key = t;
              } else if (p.name.text === 'aliases' && ts.isArrayLiteralExpression(p.initializer)) {
                for (const el of p.initializer.elements) {
                  const t = literalText(el, ts);
                  if (typeof t === 'string') aliasesArr.push(t);
                }
              }
            }
          }
          if (key !== undefined && aliasesArr.length > 0) {
            aliases.set(key, aliasesArr);
          }
        }
      }
    }
  }
  const rulesSource = sources.find((s) => s.rel === 'packages/settings/src/settings/settingsRegistry.ts');
  let normalizationRules = {};
  if (rulesSource) {
    const sf = readSource(ts, rulesSource.abs);
    for (const st of sf.statements) {
      if (!ts.isVariableStatement(st)) continue;
      for (const decl of st.declarationList.declarations) {
        if (!ts.isIdentifier(decl.name) || decl.name.text !== 'ALIAS_NORMALIZATION_RULES') continue;
        if (ts.isObjectLiteralExpression(decl.initializer)) {
          normalizationRules = {};
          for (const p of decl.initializer.properties) {
            if (ts.isPropertyAssignment(p) && p.name && ts.isStringLiteral(p.initializer)) {
              const name = ts.isStringLiteral(p.name) ? p.name.text : p.name.text;
              if (ts.isStringLiteral(p.name) || ts.isIdentifier(p.name)) {
                normalizationRules[name] = p.initializer.text;
              }
            }
          }
        }
      }
    }
  }
  return { aliases, normalizationRules, errors };
}

function collectStringLiterals(ts, abs) {
  const sf = readSource(ts, abs);
  const out = new Set();
  function visit(node) {
    if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) out.add(node.text);
    ts.forEachChild(node, visit);
  }
  for (const st of sf.statements) visit(st);
  return out;
}

function buildClassificationTable() {
  const rows = [];
  const add = (key, classification, owner, note) => rows.push({ key, classification, owner, note });
  const wire = 'wire-applied';
  const host = 'host-applied';
  const meta = 'exact-compatibility-metadata';
  const rej = 'rejected';
  const common = 'common';
  const std = 'standard-chat';
  const ds = 'dsflash-chat';
  const resp = 'openai-responses';
  const codex = 'codex-responses';

  add('temperature', wire, std, 'finite JSON number');
  add('top_p', wire, std, 'finite JSON number');
  add('topP', wire, std, 'alias of top_p');
  add('max_tokens', wire, common, 'max-output family; normalized common integer');
  add('max-tokens', wire, common, 'alias of max_tokens (max-output family)');
  add('maxTokens', wire, common, 'alias of max_tokens (max-output family)');
  add('max_output_tokens', wire, common, 'max-output family; alias max-output-tokens');
  add('max-output-tokens', wire, common, 'alias of max_output_tokens');
  add('maxOutputTokens', wire, common, 'max-output family; alias max-output');
  add('max-output', wire, common, 'alias of maxOutputTokens');
  add('maxOutput', wire, common, 'Rust-only max-output spelling; normalized common integer');
  add('seed', wire, std, 'nonnegative integer; Standard Chat only');
  add('stream-first-response-timeout-ms', host, std, 'nonnegative integer; alias streamFirstResponseTimeoutMs');
  add('streamFirstResponseTimeoutMs', host, std, 'alias of stream-first-response-timeout-ms');
  add('stream-idle-timeout-ms', host, ds, 'nonnegative integer; dsflash; Codex accepts only 0');
  add('streamIdleTimeoutMs', host, ds, 'alias of stream-idle-timeout-ms');
  add('auth-key', host, common, 'inline API-key credential source; Codex rejects');
  add('authKey', host, common, 'alias of auth-key');
  add('api-key', host, common, 'alias of auth-key; newly accepted non-Codex');
  add('apiKey', host, common, 'alias of auth-key');
  add('auth-keyfile', host, common, 'keyfile credential source; Codex rejects');
  add('authKeyfile', host, common, 'alias of auth-keyfile');
  add('api-keyfile', host, common, 'alias of auth-keyfile; newly accepted non-Codex');
  add('apiKeyfile', host, common, 'alias of auth-keyfile');
  add('auth-key-name', host, common, 'named provider-key credential source; resolves from the credential env selector then the secure store');
  add('base-url', host, common, 'API-specific endpoint alias; Codex requires fixed endpoint');
  add('baseUrl', host, common, 'alias of base-url');
  add('baseURL', host, common, 'alias of base-url');
  add('user-agent', rej, common, 'credential-bearing custom-header key rejects');
  add('User-Agent', rej, common, 'alias of user-agent; rejects');
  add('custom-headers', rej, common, 'custom headers reject');
  add('auth.noBrowser', meta, common, 'accepted exact metadata; registry defines it twice');
  add('authOnly', meta, common, 'accepted exact compatibility metadata');
  add('apiMode', host, common, 'selector chat|responses; exact lowercase untrimmed');
  add('responsesMode', host, common, 'selector chat|responses; precedence after apiMode');
  add('responses-mode', host, common, 'selector chat|responses; precedence after responsesMode');
  add('openaiResponsesEnabled', meta, common, 'inert validated boolean metadata');
  add('requires-auth', meta, common, 'accepted exact compatibility metadata');
  add('model', meta, common, 'validated bounded model identifier (top-level field)');
  add('defaultModel', rej, common, 'rejected; not an accepted profile key');
  add('enabled', meta, common, 'accepted exact compatibility metadata');
  add('toolFormat', host, common, 'tool-format owner; alias tool-format');
  add('tool-format', host, common, 'alias of toolFormat');
  add('toolFormatOverride', rej, common, 'toolFormatOverride rejects');
  add('tool-format-override', rej, common, 'alias of toolFormatOverride; rejects');
  add('tool_choice', rej, common, 'tool-choice/toolChoice aliases; reject');
  add('tool-choice', rej, common, 'alias of tool_choice; reject');
  add('toolChoice', rej, common, 'alias of tool_choice; reject');
  add('response_format', rej, common, 'response-format/responseFormat aliases; reject');
  add('response-format', rej, common, 'alias of response_format; reject');
  add('responseFormat', rej, common, 'alias of response_format; reject');
  add('logit_bias', rej, common, 'rejected model-param');
  add('api-version', rej, common, 'rejected');
  add('GOOGLE_CLOUD_PROJECT', rej, common, 'rejected');
  add('GOOGLE_CLOUD_LOCATION', rej, common, 'rejected');
  add('context-limit', host, common, 'host context limit; Codex exact 262144 predicate');
  add('contextLimit', host, common, 'ephemeralSettings.contextLimit alias');
  add('maxTurnsPerPrompt', host, common, 'effective round cap: -1 or 1..=32');
  add('loopDetectionEnabled', host, common, 'exact false accepted; other values reject');
  add('toolCallLoopThreshold', rej, common, 'rejected');
  add('contentLoopThreshold', rej, common, 'rejected');
  add('retries', rej, common, 'rejected');
  add('retrywait', rej, common, 'rejected');
  add('auth-retry-timeout', rej, common, 'rejected');
  add('socket-timeout', rej, common, 'rejected');
  add('socket-keepalive', rej, common, 'rejected');
  add('socket-nodelay', rej, common, 'rejected');
  add('todo-continuation', rej, common, 'rejected');
  add('stream-options', rej, common, 'rejected');
  add('enable-tool-prompts', rej, common, 'rejected');
  add('emojifilter', meta, common, 'exact auto metadata');
  add('dumponerror', rej, common, 'dumponerror rejects');
  add('dumpcontext', rej, common, 'dumpcontext rejects');
  add('tools.disabled', host, common, 'exact bounded string array; disabled-tools alias');
  add('disabled-tools', host, common, 'alias of tools.disabled; equality required');
  add('tools.allowed', meta, common, 'empty array only; nonempty allowlist rejects');
  add('shell-replacement', rej, ds, 'dsflash boolean metadata; sibling string values allowlist/all/none/true/false reject');
  add('streaming', rej, common, 'fixed rejection of streaming');
  add('compression-threshold', rej, common, 'rejected');
  add('compression.strategy', rej, common, 'rejected');
  add('compression.profile', rej, common, 'rejected');
  add('compression.density.readWritePruning', rej, common, 'rejected');
  add('compression.density.fileDedupe', rej, common, 'rejected');
  add('compression.density.recencyPruning', rej, common, 'rejected');
  add('compression.density.recencyRetention', rej, common, 'rejected');
  add('compression.density.compressHeadroom', rej, common, 'rejected');
  add('compression.density.optimizeThreshold', rej, common, 'rejected');
  add('kimi.experimental-video', rej, common, 'rejected');
  add('mcp.lazy', rej, common, 'rejected');
  add('mcp.eagerServers', rej, common, 'rejected');
  add('task-default-timeout-seconds', meta, common, 'exact 3600 metadata');
  add('task-max-timeout-seconds', meta, common, 'exact 7200 metadata');
  add('task-max-async', rej, common, 'rejected');
  add('shell-max-background-jobs', rej, common, 'rejected');
  add('shell-background-log-max-bytes', rej, common, 'rejected');
  add('shell-default-timeout-seconds', rej, common, 'rejected');
  add('shell-max-timeout-seconds', rej, common, 'rejected');
  add('shell-inactivity-timeout-seconds', rej, common, 'rejected');
  add('shell-output-retention-max-bytes', rej, common, 'rejected');
  add('subagents.async.enabled', rej, common, 'rejected');
  add('token-usage-log', rej, common, 'rejected');
  add('max-prompt-tokens', rej, common, 'rejected');
  add('tool-output-max-items', rej, common, 'rejected');
  add('tool-output-max-tokens', rej, common, 'rejected');
  add('tool-output-truncate-mode', rej, common, 'rejected');
  add('tool-output-item-size-limit', rej, common, 'rejected');
  add('file-read-max-lines', rej, common, 'rejected');
  add('image-resize.enabled', rej, common, 'rejected image-sizing key');
  add('image-resize.maxLongEdge', rej, common, 'rejected image-sizing key');
  add('image-resize.maxShortEdge', rej, common, 'rejected image-sizing key');
  add('image-resize.maxPixels', rej, common, 'rejected image-sizing key');
  add('max-image-dimension', rej, common, 'rejected image-sizing key');
  add('max-image-pixels', rej, common, 'rejected image-sizing key');
  add('prompt-caching', meta, resp, 'responses/codex cache mode off|1h|24h; 5m rejects');
  add('responses-stateful', rej, resp, 'fixed unsupported-stateful-Responses diagnostic');
  add('media.pdf.enabled', rej, common, 'rejected');
  add('rate-limit-throttle', rej, common, 'rejected');
  add('rate-limit-throttle-threshold', rej, common, 'rejected');
  add('rate-limit-max-wait', rej, common, 'rejected');
  add('sandbox-base-url', rej, common, 'rejected');
  add('reasoning', rej, common, 'nested reasoning object rejects');
  add('reasoning.enabled', meta, std, 'reasoning enablement metadata');
  add('reasoning.effort', meta, std, 'effort enum; dsflash optional wire effort');
  add('reasoning.includeInResponse', meta, std, 'accepted metadata; inverted stateless behavior');
  add('reasoning.includeInContext', meta, std, 'accepted metadata; inverted stateless behavior');
  add('reasoning.stripFromContext', meta, ds, 'exact string; dsflash marker');
  add('reasoning.summary', wire, resp, 'summary enum; codex requires auto');
  add('reasoning.effortMap', rej, common, 'rejected');
  add('reasoning.enabledMap', rej, common, 'rejected');
  add('reasoning.maxTokens', rej, common, 'rejected');
  add('reasoning.budgetTokens', rej, common, 'rejected');
  add('reasoning.adaptiveThinking', meta, codex, 'codex requires true');
  add('reasoning.effortWireFormat', rej, common, 'rejected');
  add('reasoning.enabledWireFormat', rej, common, 'rejected');
  add('reasoning.format', rej, common, 'rejected');
  add('reasoning.fieldName', rej, common, 'rejected');
  add('reasoning.update', rej, common, 'rejected');
  add('reasoning.display', rej, common, 'rejected');
  // Registry keys with no applied behavior in this runtime: the fixed rejected
  // template, same family as retries/socket-timeout.
  add('circuit_breaker_enabled', rej, common, 'rejected circuit-breaker registry key');
  add('circuit_breaker_failure_threshold', rej, common, 'rejected circuit-breaker registry key');
  add('circuit_breaker_failure_window_ms', rej, common, 'rejected circuit-breaker registry key');
  add('circuit_breaker_recovery_timeout_ms', rej, common, 'rejected circuit-breaker registry key');
  add('frequency_penalty', rej, common, 'rejected sampling registry key');
  add('include-folder-structure', rej, common, 'rejected');
  add('model.allMemoriesAreCore', rej, common, 'rejected');
  add('presence_penalty', rej, common, 'rejected sampling registry key');
  add('stop', rej, common, 'rejected');
  add('timeout_ms', rej, common, 'rejected');
  add('top_k', rej, common, 'chat transport cannot serialize top_k; rejected');
  add('tpm_threshold', rej, common, 'rejected');
  add('modelParams.provider', rej, common, 'nested modelParams.provider rejects');
  add('modelParams.parse_reasoning', rej, ds, 'parse_reasoning rejects');
  add('modelParams.clear_thinking', rej, ds, 'clear_thinking rejects');
  add('modelParams.chat_template_kwargs', wire, ds, 'required discriminator object enable_thinking bool');
  add('text.verbosity', wire, codex, 'codex requires medium; responses optional');
  add('type', rej, common, 'exact standard accepted; loadbalancer rejects');
  add('version', meta, common, 'exact integer 1; omission accepted');
  add('provider', host, common, 'provider resolution; unsupported providers reject');
  add('name', meta, common, 'bounded inert profile metadata');
  add('_note', meta, common, 'bounded inert profile metadata');
  add('policy', rej, common, 'load-balancer container key rejects');
  add('profiles', rej, common, 'load-balancer container key rejects');
  add('loadBalancer', rej, common, 'explicit loadBalancer container rejects');
  add('ephemeralSettings.contextLimit', rej, common, 'top-level load-balancer contextLimit rejects (distinct from accepted nested alias)');
  add('auth.type', rej, common, 'top-level auth container member rejects');
  return rows;
}

// ---------------------------------------------------------------------------
// Profile shape / redaction helpers.
// ---------------------------------------------------------------------------
function topLevelKeysOf(profile) {
  return Object.keys(profile);
}

function jsonTypeOf(value) {
  if (value === null) return 'null';
  if (Array.isArray(value)) return 'array';
  switch (typeof value) {
    case 'string':
      return 'string';
    case 'boolean':
      return 'boolean';
    case 'number':
      return 'number';
    default:
      return 'object';
  }
}

function shapeOf(value) {
  if (Array.isArray(value)) return { arrayOf: [...new Set(value.map((v) => jsonTypeOf(v)))].sort() };
  if (value !== null && typeof value === 'object') {
    const o = {};
    for (const k of Object.keys(value)) o[k] = shapeOf(value[k]);
    return o;
  }
  return jsonTypeOf(value);
}

function normalizedSpelling(spelling, aliases, rules) {
  if (aliases.has(spelling)) return aliases.get(spelling);
  for (const [canonical, list] of aliases) {
    if (list.includes(spelling)) return canonical;
  }
  return rules[spelling] ?? spelling;
}

function isSecretLikePath(ownerSpelling) {
  const last = String(ownerSpelling).split('.').pop() ?? '';
  if (/^auth/i.test(last) && /(key|token|file)/i.test(last)) return true;
  return SECRET_LIKE_LAST_SEGMENT.test(last);
}

function isAllowedLiteralString(ownerSpelling, value) {
  if (ownerSpelling === 'base-url' || ownerSpelling === 'baseUrl' || ownerSpelling === 'baseURL') {
    return true;
  }
  if (typeof value === 'string' && (value.startsWith('/') || value.startsWith('~'))) return false;
  return true;
}

function redactValueForKey(spelling, value, aliases, rules) {
  const ph = CREDENTIAL_SOURCE_PLACEHOLDERS.get(spelling);
  if (ph !== undefined) return ph;
  const normalized = normalizedSpelling(spelling, aliases, rules);
  const phN = CREDENTIAL_SOURCE_PLACEHOLDERS.get(normalized);
  if (phN !== undefined) return phN;
  if (typeof value !== 'string') return value;
  if (/-(keyfile|key-file)$|Keyfile$|keyfile$/i.test(normalized)) return PLACEHOLDER_KEYFILE;
  if (isSecretLikePath(normalized)) return PLACEHOLDER_CREDENTIAL;
  if (isAllowedLiteralString(normalized, value)) return value;
  return PLACEHOLDER_PATH;
}

function redactTree(spelling, value, aliases, rules) {
  if (Array.isArray(value)) return value.map((v) => redactTree(spelling, v, aliases, rules));
  if (value !== null && typeof value === 'object' && !Array.isArray(value)) {
    const o = {};
    for (const k of Object.keys(value)) {
      const child = spelling === '' ? k : `${spelling}.${k}`;
      o[k] = redactTree(child, value[k], aliases, rules);
    }
    return o;
  }
  return redactValueForKey(spelling, value, aliases, rules);
}

function redactProfile(name, profile) {
  return redactTree(name, profile, DUMMY_ALIASES, DUMMY_RULES);
}
const DUMMY_ALIASES = new Map();
const DUMMY_RULES = {};

function stableJson(value) {
  return JSON.stringify(value, null, 2) + '\n';
}

function extractPaths(profile) {
  const out = [];
  const walk = (p, v) => {
    if (v !== null && typeof v === 'object' && !Array.isArray(v)) {
      for (const k of Object.keys(v)) walk([...p, k], v[k]);
    } else if (Array.isArray(v)) {
      for (const c of v) walk(p, c);
    } else {
      out.push(p.join('.'));
    }
  };
  walk([], profile);
  return out.sort();
}

function expectDisposition(file, provider) {
  if (provider === 'codex') {
    return { stage: 'in-scope installed codex', target: 'codex-responses-http', plaintextOptIn: true };
  }
  if (provider === 'anthropic') {
    return { stage: 'in-scope installed anthropic', target: 'anthropic-messages', plaintextOptIn: true };
  }
  return { stage: 'in-scope installed openai', target: 'target-resolved', plaintextOptIn: true };
}

// ---------------------------------------------------------------------------
// Synthetic fixtures (deterministic staged cases; task 2/3 plan-prose ladders).
// ---------------------------------------------------------------------------
function stagedLadderBuilders() {
  return [
    {
      base: 'friendliglm.json',
      name: 'friendliglm.accepted-route.synthetic.json',
      build: (r) => ({
        ...r,
        modelParams: { ...(r.modelParams ?? {}) },
        ephemeralSettings: { ...(r.ephemeralSettings ?? {}), 'base-url': 'https://api.friendli.ai/v1' },
      }),
    },
    {
      base: 'friendliglm.json',
      name: 'friendliglm.without-auth-key-name.synthetic.json',
      build: (r) => {
        const es = { ...(r.ephemeralSettings ?? {}), 'base-url': 'https://api.friendli.ai/v1' };
        delete es['auth-key-name'];
        return { ...r, modelParams: { ...(r.modelParams ?? {}) }, ephemeralSettings: es };
      },
    },
    {
      base: 'friendliglm.json',
      name: 'friendliglm.settings-accepted.synthetic.json',
      build: (r) => {
        const es = { ...(r.ephemeralSettings ?? {}), 'base-url': 'https://api.friendli.ai/v1' };
        delete es['auth-key-name'];
        // Settings-accepted rung: enable_thinking only (no reasoning_effort), and
        // no unsupported model parameters.
        const mp = { temperature: 0.6, chat_template_kwargs: { enable_thinking: true } };
        return { ...r, modelParams: mp, ephemeralSettings: es };
      },
    },
    {
      base: 'chutesk2streaming.json',
      name: 'chutesk2streaming.with-discriminator.synthetic.json',
      build: (r) => ({
        ...r,
        modelParams: { chat_template_kwargs: { enable_thinking: false } },
        ephemeralSettings: { ...(r.ephemeralSettings ?? {}), 'reasoning.stripFromContext': 'none' },
      }),
    },
    {
      base: 'chutesk2streaming.json',
      name: 'chutesk2streaming.streaming-only.synthetic.json',
      build: (r) => {
        // Streaming-only: every dsflash marker is removed so the unsupported
        // `streaming` key is the first (class 6) failure without a discriminator.
        const es = { ...(r.ephemeralSettings ?? {}) };
        for (const k of [
          'shell-replacement',
          'reasoning.enabled',
          'reasoning.includeInResponse',
          'reasoning.includeInContext',
          'reasoning.stripFromContext',
        ]) {
          delete es[k];
        }
        return { ...r, modelParams: { ...(r.modelParams ?? {}) }, ephemeralSettings: es };
      },
    },
    {
      base: 'crusoeglm.json',
      name: 'crusoeglm.without-auth-key-name.synthetic.json',
      build: (r) => {
        const es = { ...(r.ephemeralSettings ?? {}) };
        delete es['auth-key-name'];
        return { ...r, modelParams: { ...(r.modelParams ?? {}) }, ephemeralSettings: es };
      },
    },
    {
      base: 'crusoeglm.json',
      name: 'crusoeglm.with-discriminator.synthetic.json',
      build: (r) => {
        const es = { ...(r.ephemeralSettings ?? {}) };
        delete es['auth-key-name'];
        return { ...r, modelParams: { temperature: 0.3, top_p: 0.9, chat_template_kwargs: { enable_thinking: false } }, ephemeralSettings: es };
      },
    },
    {
      base: 'crusoeglm.json',
      name: 'crusoeglm.without-prompt-caching.synthetic.json',
      build: (r) => {
        const es = { ...(r.ephemeralSettings ?? {}) };
        delete es['auth-key-name'];
        delete es['prompt-caching'];
        return { ...r, modelParams: { temperature: 0.3, chat_template_kwargs: { enable_thinking: false }, top_p: '0.8' }, ephemeralSettings: es };
      },
    },
    {
      base: 'ollamakimi.json',
      name: 'ollamakimi.without-auth-key-name.synthetic.json',
      build: (r) => {
        const es = { ...(r.ephemeralSettings ?? {}) };
        delete es['auth-key-name'];
        return { ...r, ephemeralSettings: es };
      },
    },
    {
      base: 'ollamakimi.json',
      name: 'ollamakimi.standard-summary.synthetic.json',
      build: (r) => {
        const es = { ...(r.ephemeralSettings ?? {}) };
        for (const k of [
          'auth-key-name',
          'reasoning.enabled',
          'reasoning.includeInResponse',
          'reasoning.includeInContext',
          'reasoning.stripFromContext',
        ]) {
          delete es[k];
        }
        return { ...r, ephemeralSettings: es };
      },
    },
    {
      base: 'gpt56solhigh.json',
      name: 'gpt56solhigh.type-standard.synthetic.json',
      build: (r) => ({ ...r, type: 'standard' }),
    },
    {
      base: 'gpt56solhigh.json',
      name: 'gpt56solhigh.without-optional-metadata.synthetic.json',
      build: (r) => {
        const es = { ...(r.ephemeralSettings ?? {}) };
        for (const k of ['stream-idle-timeout-ms', 'task-default-timeout-seconds', 'task-max-timeout-seconds']) {
          delete es[k];
        }
        return { ...r, ephemeralSettings: es };
      },
    },
  ];
}

function omitFixtures() {
  const list = [];
  for (const field of ['reasoning.adaptiveThinking', 'reasoning.includeInResponse', 'reasoning.includeInContext', 'reasoning.stripFromContext']) {
    list.push({
      base: 'gpt56solhigh.json',
      name: `gpt56solhigh.omit-${field.split('.').pop()}.synthetic.json`,
      build: (r) => {
        const es = { ...(r.ephemeralSettings ?? {}) };
        delete es[field];
        return { ...r, ephemeralSettings: es };
      },
    });
  }
  return list;
}

// ---------------------------------------------------------------------------
// Markdown rendering (deterministic).
// ---------------------------------------------------------------------------
function renderMarkdown(artifact) {
  const c = artifact.counts;
  const L = [];
  const push = (s = '') => L.push(s);
  push('# Profile compatibility inventory (Phase 0)');
  push();
  push('This file is the checked-in exhaustive compatibility inventory for the Phase 0');
  push('profile-fixture and compatibility-inventory tasks. It is generated by the operator');
  push('and is not a CI or release gate:');
  push();
  push('```sh');
  push('node scripts/generate-profile-compatibility-inventory.mjs \\');
  push('    --sibling <absolute-llxprt-code-checkout> --profiles <absolute-profile-directory>');
  push('```');
  push();
  push(`Sibling lockfile-pinned TypeScript compiler version: \`${artifact.generator.siblingTypescriptVersion}\`.`);
  push();
  push('The generator uses the sibling compiler AST to traverse every `persistToProfile: true`');
  push('entry in source order and to statically resolve string literals, literal array/object');
  push('spreads, the two present `.map(ArrowFunction)` producers, and spreads of imported');
  push('`const` string-literal arrays such as `COMPRESSION_STRATEGIES`. Every other computed');
  push('expression fails generation. Values are key paths and JSON types only (frozen safe');
  push('enums and fixed redacted placeholders). This file records no scalar credential, account');
  push('id, token, local path, or credential-store value.');
  push();
  push('The checked-in `tests/fixtures/profile-compatibility-inventory.json` is the exhaustive');
  push(`authority for the disposition of the ${c.installedInScope} installed in-scope profiles (`);
  push(`\`${c.inScopeOpenAi}\` provider \`openai\`, \`${c.inScopeAnthropic}\` provider \`anthropic\`, plus`);
  push(`\`gpt56solhigh.json\` provider \`codex\`). Every tracked fixture field has`);
  push('exactly one classification and one owner. The repository self-tests in');
  push('`tests/profile_compatibility.rs` fail if the inventory is internally inconsistent or if');
  push('any fixture field is unclassified or multiply owned.');
  push();
  push('## Issue 1 OpenAI Responses extension');
  push();
  push('Public OpenAI Responses profiles select HTTP Responses with either provider `openai-responses`');
  push('or provider `openai` plus a Responses selector. They use API-key credentials, send a complete');
  push('transcript on every round, force `store: false`, and never send `previous_response_id`. Endpoint');
  push('paths are limited to an origin, `/v1`, `/responses`, or `/v1/responses`, with one optional trailing');
  push('slash.');
  push();
  push('Responses reasoning requires `reasoning.enabled: true`, effort `low`, `medium`, or `high`, and');
  push('summary `concise`, `detailed`, or `auto`. Optional `text.verbosity` uses the same three levels.');
  push('Omitted `prompt-caching`, `1h`, and `24h` send the validated session label as');
  push('`prompt_cache_key` with retention `24h`; `off` omits both fields. Session labels therefore leave');
  push('the process in cached mode and must not contain project or secret information. The literal');
  push('`default` is used when the CLI session option is omitted. Codex WebSocket is separate and sends');
  push('neither that key nor a session header.');
  push();
  push(`## Persistable-key inventory`);
  push();
  push(`- Total persistable entries: ${c.inventoryTotalEntries}`);
  push(`- Distinct keys: ${c.inventoryDistinctKeys}`);
  push(`- Duplicate extra entries: ${c.duplicateExtraEntries}`);
  push();
  if (artifact.inventory.duplicateGroups.length > 0) {
    push('Duplicate definitions:');
    push();
    push('| key | count | sources |');
    push('| --- | --- | --- |');
    for (const g of artifact.inventory.duplicateGroups) {
      push(`| \`${g.key}\` | ${g.count} | ${g.files.map((f) => '`' + f + '`').join(', ')} |`);
    }
    push();
  }
  push('`ALIAS_NORMALIZATION_RULES` and per-spec `aliases`:');
  push();
  push('| canonical | type | aliases | application paths |');
  push('| --- | --- | --- | --- |');
  for (const e of artifact.inventory.entries) {
    const sources = (e.applicationPaths ?? []).map((a) => a.split('/').pop()).join(', ');
    push(`| \`${e.key}\` | ${e.type ?? 'declared'} | ${(e.aliases ?? []).map((a) => '`' + a + '`').join(', ')} | ${sources} |`);
  }
  push();
  push(`## In-scope installed profile disposition (${c.installedInScope} fixtures)`);
  push();
  push('| fixture | provider | expected disposition |');
  push('| --- | --- | --- |');
  for (const r of artifact.profiles.installedInScope) {
    push(`| \`${r.file}\` | ${r.provider} | ${r.expectedDisposition.stage} |`);
  }
  push();
  push(`## Load-balancer shapes (${c.loadBalancer} rows; out of target construction scope)`);
  push();
  push('| fixture | reason |');
  push('| --- | --- | --- |');
  for (const lb of artifact.profiles.loadBalancer) {
    push(`| \`${lb.file}\` | ${lb.reason} |`);
  }
  push();
  push(`## Unsupported-provider shapes (${c.unsupportedProviders} groups; exact provider-resolution rejection)`);
  push();
  push('| provider | files | reason |');
  push('| --- | --- | --- |');
  for (const u of artifact.profiles.unsupportedProviders) {
    push(`| \`${u.provider}\` | ${u.files.map((f) => '`' + f + '`').join(', ')} | ${u.reason} |`);
  }
  push();
  push(`## Synthetic staged fixtures (${artifact.profiles.synthetic.length})`);
  push();
  push('| fixture | kind | expected outcome |');
  push('| --- | --- | --- |');
  for (const s of artifact.profiles.synthetic) {
    push(`| \`${s.file}\` | ${s.kind} | ${s.expectedOutcome} |`);
  }
  push();
  return L.join('\n');
}

// ---------------------------------------------------------------------------
// Self-test mode.
// ---------------------------------------------------------------------------
function runSelfTest() {
  const failures = [];
  const pass = (cond, label) => {
    if (!cond) failures.push(label);
    process.stdout.write((cond ? 'PASS: ' : 'FAIL: ') + label + '\n');
  };
  const siblingRoot = process.env.LLXPRT_SIBLING_CHECKOUT || null;
  if (!siblingRoot || !fs.existsSync(path.join(siblingRoot, 'package.json'))) {
    pass(
      false,
      'LLXPRT_SIBLING_CHECKOUT must point at the sibling LLxprt Code checkout for computed-form self-tests',
    );
    process.exit(1);
  }
  const { ts, version } = loadTypeScript(siblingRoot);
  pass(true, `lockfile-pinned typescript resolved ${version}`);

  const manualConsts = new Map([['COMPRESSION_STRATEGIES', ['middle-out', 'top-down']]]);
  const good = ts.createSourceFile(
    'good.ts',
    [
      "import { COMPRESSION_STRATEGIES } from './registry-types';",
      'export const R = [',
      "{ key: 'a', type: 'number', persistToProfile: true },",
      "...[{ key: 'x-1' }, { key: 'y-2' }].map(({ key }) => ({ key, type: 'number', persistToProfile: true })),",
      "{ key: 'c', type: 'enum', enumValues: [...COMPRESSION_STRATEGIES], persistToProfile: true },",
      '];',
    ].join('\n'),
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TS,
  );
  const goodSpecs = [];
  const goodErrors = [];
  const badSpecs = [];
  const forEachSpread = (sf, cb) => {
    const visit = (node) => {
      if (node && ts.isSpreadElement(node)) {
        cb(node);
        return;
      }
      ts.forEachChild(node, visit);
    };
    for (const st of sf.statements) visit(st);
  };
  forEachSpread(good, (spread) => {
    if (spread.expression && ts.isIdentifier(spread.expression) && manualConsts.has(spread.expression.text)) {
      for (const v of manualConsts.get(spread.expression.text)) goodSpecs.push(v);
      return;
    }
    const r = expandSpread(spread.expression, ts, good, (m) => goodErrors.push(m));
    if (r.kind === 'specs') for (const s of r.value) goodSpecs.push(s.key);
  });
  pass(goodErrors.length === 0, 'supported map/import-spread producers accept');

  const badErrors = [];
  for (const code of [
    'export const R = [...(createKeys()), { key: "a", persistToProfile: true }];',
    'export const R = [...["a","b"].map((k, i) => ({ key: "x" + i, type: "number", persistToProfile: true }))];',
    'export const R = [...["c"].map((k) => ({ key: k.replace(/c/, "d"), persistToProfile: true }))];',
    'const k = "x"; export const R = [{ [k]: 1 }];',
  ]) {
    const sf = ts.createSourceFile('bad.ts', code, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
    forEachSpread(sf, (spread) => {
      const r = expandSpread(spread.expression, ts, sf, (m) => badErrors.push(m));
      if (r.kind === 'specs') for (const s of r.value) badSpecs.push(s.key);
    });
  }
  pass(
    goodSpecs.join(',') === 'x-1,y-2,middle-out,top-down',
    'supported producers resolve literal keys (' + goodSpecs.join(',') + ')',
  );
  pass(badErrors.length >= 3, 'unsupported computed expressions reject generation (' + badErrors.length + ' diagnostic(s))');

  pass(goodErrors.length === 0, 'duplicate good errors remain zero');

  const r0 = redactValueForKey('auth-key', 'sk-installed-secret', DUMMY_ALIASES, DUMMY_RULES);
  const r1 = redactValueForKey('auth-keyfile', '/Users/alice/.llxprt/keys/k', DUMMY_ALIASES, DUMMY_RULES);
  const r2 = redactValueForKey('reasoning.effort', 'high', DUMMY_ALIASES, DUMMY_RULES);
  const r3 = redactValueForKey('base-url', 'https://chatgpt.com/backend-api/codex', DUMMY_ALIASES, DUMMY_RULES);
  pass(r0 === PLACEHOLDER_CREDENTIAL, 'inline credential becomes a fixed placeholder');
  pass(r1 === PLACEHOLDER_KEYFILE, 'keyfile path becomes a fixed placeholder');
  pass(r2 === 'high', 'safe enum literal persists exactly');
  pass(r3 === 'https://chatgpt.com/backend-api/codex', 'approved endpoint persists verbatim');

  if (fs.existsSync(ARTIFACT_PATH)) {
    const art = readJson(ARTIFACT_PATH);
    const inv = art.inventory || {};
    const entries = inv.entries || [];
    const distinct = new Set(entries.map((e) => e.key));
    pass(inv.totalEntries === entries.length, `artifact totalEntries == derived entry count (${entries.length})`);
    pass(inv.distinctKeys === distinct.size, `artifact distinctKeys == derived distinct count (${distinct.size})`);
    const counts = {};
    for (const e of entries) counts[e.key] = (counts[e.key] || 0) + 1;
    const owners = new Map();
    for (const c of art.classifications || []) owners.set(c.key, (owners.get(c.key) || 0) + 1);
    const multi = [...owners.values()].filter((n) => n !== 1).length;
    pass(multi === 0, 'every classification key has exactly one owner');
    const dupCount = [...Object.values(counts)].filter((n) => n > 1).length;
    pass(
      inv.distinctKeys + dupCount === entries.length,
      'duplicate extra entries == entries - distinct',
    );
    const classKeys = new Set((art.classifications || []).map((c) => c.key));
    const aliasKeys = new Set(Object.keys(inv.aliasNormalizationRules || {}));
    for (const e of entries) for (const a of e.aliases || []) aliasKeys.add(a);
    const unclassified = [...distinct].filter((k) => !classKeys.has(k) && !aliasKeys.has(k));
    pass(
      unclassified.length === 0,
      'every distinct inventory entry key has a classification row (missing: ' + unclassified.join(',') + ')',
    );
  } else {
    pass(false, 'checked-in tests/fixtures/profile-compatibility-inventory.json present');
  }

  if (failures.length > 0) {
    console.error(`self-test failed: ${failures.length} failure(s)`);
    process.exit(1);
  }
  process.stdout.write('self-test ok\n');
}

// ---------------------------------------------------------------------------
// Main generation.
// ---------------------------------------------------------------------------
function generate({ siblingRoot, profilesDir }) {
  if (!fs.existsSync(siblingRoot) || !fs.existsSync(profilesDir)) {
    console.error('error: --sibling and --profiles must name existing absolute directories');
    process.exit(2);
  }
  const { ts, version, entry } = loadTypeScript(siblingRoot);
  const sources = siblingSources(siblingRoot);
  for (const s of sources) {
    if (!fs.existsSync(s.abs)) {
      console.error(`error: missing sibling source: ${s.rel}`);
      process.exit(2);
    }
  }

  const typeSource = readSource(
    ts,
    sources.find((s) => s.rel === 'packages/settings/src/settings/registry/registry-types.ts').abs,
  );
  const { entries, errors } = deriveEntries(ts, typeSource, sources);
  if (errors.length > 0) {
    for (const e of errors) console.error('error: ' + e);
    process.exit(1);
  }
  const { aliases, normalizationRules } = deriveAliasesAndRules(ts, sources);

  const distinctKeys = [...new Set(entries.map((e) => e.key))];
  const byKey = new Map();
  for (const e of entries) {
    if (!byKey.has(e.key)) byKey.set(e.key, []);
    byKey.get(e.key).push(e);
  }
  const duplicateGroups = [];
  for (const [key, list] of byKey) {
    if (list.length > 1) duplicateGroups.push({ key, count: list.length, files: list.map((e) => e.file) });
  }
  duplicateGroups.sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
  duplicateGroups.forEach((g) => g.files.sort());

  const existing = sources.map((s) => ({ rel: s.rel, literals: collectStringLiterals(ts, s.abs) }));
  const appPathsFor = (needle) =>
    existing
      .filter((s) => s.literals.has(needle))
      .map((s) => s.rel)
      .sort();

  const inventoryEntries = entries.map((e) => ({
    key: e.key,
    type: e.type,
    source: e.file,
    aliases: [...(aliases.get(e.key) ?? [])],
    applicationPaths: appPathsFor(e.key),
  }));

  const classifications = buildClassificationTable();

  const profileFiles = fs
    .readdirSync(profilesDir)
    .filter((f) => f.endsWith('.json') && !/\.bak-/.test(f))
    .sort();

  const inScope = [];
  const anthropic = [];
  const loadBalancer = [];
  const codex = [];
  const unsupported = new Map();
  for (const f of profileFiles) {
    const parsed = readJson(path.join(profilesDir, f));
    const provider = typeof parsed.provider === 'string' ? parsed.provider : '';
    const type = typeof parsed.type === 'string' ? parsed.type : undefined;
    if (type === 'loadbalancer') {
      loadBalancer.push({
        file: f,
        provider,
        type: 'loadbalancer',
        reason: 'fixed unsupported-load-balancing diagnostic',
        topLevel: topLevelKeysOf(parsed),
        structure: shapeOf(parsed),
      });
      continue;
    }
    if (provider === 'codex') {
      codex.push({ file: f, provider, topLevel: topLevelKeysOf(parsed), structure: shapeOf(parsed) });
      continue;
    }
    if (provider === 'openai') {
      inScope.push({ file: f, provider, topLevel: topLevelKeysOf(parsed), structure: shapeOf(parsed) });
      continue;
    }
    if (provider === 'anthropic') {
      anthropic.push({ file: f, provider, topLevel: topLevelKeysOf(parsed), structure: shapeOf(parsed) });
      continue;
    }
    if (!unsupported.has(provider)) unsupported.set(provider, { provider, files: [], structures: [], reason: 'exact provider-resolution rejection' });
    unsupported.get(provider).files.push(f);
    unsupported.get(provider).structures.push(shapeOf(parsed));
  }
  for (const u of unsupported.values()) u.files.sort();
  const unsupportedProviders = [...unsupported.values()].sort((a, b) => (a.provider < b.provider ? -1 : a.provider > b.provider ? 1 : 0));

  const counts = {
    profileJsonFiles: profileFiles.length,
    inScopeOpenAi: inScope.length,
    inScopeCodex: codex.length,
    inScopeAnthropic: anthropic.length,
    installedInScope: inScope.length + codex.length + anthropic.length,
    loadBalancer: loadBalancer.length,
    unsupportedProviders: unsupportedProviders.length,
    inventoryTotalEntries: entries.length,
    inventoryDistinctKeys: distinctKeys.length,
    duplicateGroups: duplicateGroups.length,
    duplicateExtraEntries: entries.length - distinctKeys.length,
  };

  // Redacted installed fixtures.
  fs.mkdirSync(FIXTURES_ROOT, { recursive: true });
  const installedRows = [];
  for (const p of [...inScope, ...codex, ...anthropic]) {
    const raw = readJson(path.join(profilesDir, p.file));
    const redacted = redactProfile(p.file.replace(/\.json$/, ''), raw);
    fs.writeFileSync(path.join(FIXTURES_ROOT, p.file), stableJson(redacted));
    installedRows.push({
      file: p.file,
      provider: p.provider,
      scope: p.provider,
      topLevel: topLevelKeysOf(raw),
      structure: shapeOf(raw),
      paths: extractPaths(raw),
      expectedDisposition: expectDisposition(p.file, p.provider),
    });
  }
  installedRows.sort((a, b) => (a.file < b.file ? -1 : a.file > b.file ? 1 : 0));
  const firstFailures = new Map([
    ['friendliglm.json', 'arbitrary route prefix rejects (endpoint class) before the named secure-store reference'],
    ['qwen38.json', 'named secure-store reference rejects before the structural dsflash gate'],
    ['qwen38-mi300x.json', 'missing modelParams.chat_template_kwargs discriminator names ephemeralSettings.shell-replacement'],
    ['ornith-runpod.json', 'missing modelParams.chat_template_kwargs discriminator names ephemeralSettings.stream-idle-timeout-ms'],
    ['zai.json', 'named secure-store reference rejects after Anthropic Messages target resolution'],
  ]);
  for (const row of installedRows) {
    const failure = firstFailures.get(row.file);
    if (failure) row.expectedDisposition = { ...row.expectedDisposition, firstFailure: failure };
  }

  // Synthetic fixtures.
  const synthetic = [];
  const writeSynthetic = (stage, kind, expectedOutcome) => {
    const basePath = path.join(FIXTURES_ROOT, stage.base);
    if (!fs.existsSync(basePath)) throw new Error(`synthetic ${stage.name} needs installed fixture ${stage.base}`);
    const raw = readJson(basePath);
    const built = stage.build(JSON.parse(JSON.stringify(raw)));
    fs.writeFileSync(path.join(FIXTURES_ROOT, stage.name), stableJson(built));
    synthetic.push({ file: stage.name, kind, expectedOutcome });
  };
  for (const stage of stagedLadderBuilders()) {
    writeSynthetic(stage, 'named-staged-case', 'named plan disposition per profile-compatibility.md');
  }
  for (const stage of omitFixtures()) {
    writeSynthetic(stage, 'codex-omission', 'normalized omission diagnostic per frozen matrix');
  }
  for (const lb of loadBalancer) {
    const raw = readJson(path.join(profilesDir, lb.file));
    const target = `loadbalancer.${lb.file.replace(/\.json$/, '')}.synthetic.json`;
    fs.writeFileSync(
      path.join(FIXTURES_ROOT, target),
      stableJson(redactProfile(target.replace(/\.json$/, ''), raw)),
    );
    synthetic.push({ file: target, kind: 'load-balancer-shape', expectedOutcome: 'fixed unsupported-load-balancing diagnostic' });
  }
  {
    const authProfile = {
      version: 1,
      type: 'standard',
      provider: 'openai',
      model: 'auth-fixture-model',
      modelParams: {},
      ephemeralSettings: { 'base-url': 'https://api.openai.com/v1' },
      auth: { type: 'apikey' },
    };
    const target = 'auth.synthetic.json';
    fs.writeFileSync(path.join(FIXTURES_ROOT, target), stableJson(authProfile));
    synthetic.push({ file: target, kind: 'top-level-auth', expectedOutcome: "top-level 'auth' rejects (unsafe to ignore credential policy)" });
  }
  {
    const target = 'unsupported.bedrock.synthetic.json';
    const bedrockProfile = {
      version: 1,
      provider: 'bedrock',
      model: 'anthropic.claude-3-opus',
      modelParams: {},
      ephemeralSettings: {},
    };
    fs.writeFileSync(path.join(FIXTURES_ROOT, target), stableJson(bedrockProfile));
    synthetic.push({ file: target, kind: 'unsupported-provider-shape', expectedOutcome: 'exact provider-resolution rejection' });
  }
  {
    const target = 'zai.anthropic.synthetic.json';
    const anthropicProfile = {
      version: 1,
      provider: 'anthropic',
      model: 'glm-5.3',
      modelParams: {},
      ephemeralSettings: {
        'base-url': 'https://api.z.ai/api/anthropic',
        'auth-key-name': 'zai',
      },
    };
    fs.writeFileSync(path.join(FIXTURES_ROOT, target), stableJson(anthropicProfile));
    synthetic.push({ file: target, kind: 'anthropic-messages-shape', expectedOutcome: 'Anthropic Messages target resolves offline' });
  }
  for (const u of unsupportedProviders) {
    const f = u.files[0];
    const raw = readJson(path.join(profilesDir, f));
    const slug = u.provider.toLowerCase().replace(/[^a-z0-9]+/g, '-');
    const target = `unsupported.${slug}.synthetic.json`;
    fs.writeFileSync(
      path.join(FIXTURES_ROOT, target),
      stableJson(redactProfile(target.replace(/\.json$/, ''), raw)),
    );
    synthetic.push({ file: target, kind: 'unsupported-provider-shape', expectedOutcome: 'exact provider-resolution rejection' });
  }
  synthetic.sort((a, b) => (a.file < b.file ? -1 : a.file > b.file ? 1 : 0));

  const artifact = {
    generator: {
      name: 'scripts/generate-profile-compatibility-inventory.mjs',
      siblingTypescriptVersion: version,
      note: 'operator-run; not a CI or release gate; regenerate before candidate freeze',
    },
    counts,
    inventory: {
      totalEntries: entries.length,
      distinctKeys: distinctKeys.length,
      duplicateGroups,
      aliasNormalizationRules: { ...normalizationRules },
      entries: inventoryEntries,
    },
    profiles: {
      installedInScope: installedRows,
      loadBalancer,
      unsupportedProviders,
      synthetic,
    },
    classifications,
    security: {
      note: 'contains key paths and JSON types, frozen safe enums, and redacted placeholders only',
      classifiedKeys: classifications.length,
      ownedExactlyOnce: true,
    },
  };
  fs.writeFileSync(ARTIFACT_PATH, stableJson(artifact));
  fs.writeFileSync(DOC_PATH, renderMarkdown(artifact));

  console.log(
    [
      'generated profile-compatibility inventory',
      `  installed redacted fixtures: ${installedRows.length} (${inScope.length} openai + ${codex.length} codex + ${anthropic.length} anthropic)`,
      `  synthetic fixtures: ${synthetic.length}`,
      `  inventory entries: ${entries.length} / distinct: ${distinctKeys.length} / duplicate extra: ${entries.length - distinctKeys.length}`,
      `  load-balancer rows: ${loadBalancer.length} / unsupported-provider groups: ${unsupportedProviders.length}`,
      `  artifact: tests/fixtures/profile-compatibility-inventory.json`,
      `  docs: docs/profile-compatibility.md`,
    ].join('\n'),
  );
}

function main() {
  const argv = process.argv.slice(2);
  if (argv.includes('--self-test')) {
    runSelfTest();
    return;
  }
  const arg = (name) => {
    const i = argv.indexOf(name);
    return i >= 0 && i + 1 < argv.length ? argv[i + 1] : undefined;
  };
  const sibling = arg('--sibling');
  const profiles = arg('--profiles');
  if (!sibling || !profiles) {
    console.error(
      'usage: node scripts/generate-profile-compatibility-inventory.mjs --sibling <absolute-llxprt-code-checkout> --profiles <absolute-profile-directory>\n' +
        '       node scripts/generate-profile-compatibility-inventory.mjs --self-test',
    );
    process.exit(2);
  }
  generate({ siblingRoot: path.resolve(sibling), profilesDir: path.resolve(profiles) });
}

main();
