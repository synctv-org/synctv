import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const docsRoot = path.resolve(fileURLToPath(new URL('..', import.meta.url)));
const workspaceRoot = path.resolve(docsRoot, '..');
const contentRoot = path.join(docsRoot, 'src/content/docs');
const starlightRoot = path.join(docsRoot, 'node_modules/@astrojs/starlight');

const mdxFiles = listFiles(contentRoot, (file) => file.endsWith('.mdx'));
const docsFiles = [...mdxFiles, path.join(docsRoot, 'astro.config.mjs')];
const pageUrls = new Set(mdxFiles.map(pageUrlForFile));

const linkErrors = validateInternalLinks();
const iconErrors = validateIcons();
const hygieneErrors = validateContentHygiene();
const mirrorErrors = validateEnglishMirrors();
const runtimeEnvErrors = validateRuntimeEnvironmentVariables();
const secretFileReferenceErrors = validateSecretFileReferences();
const contentWarnings = collectContentWarnings();
const errors = [
  ...linkErrors,
  ...iconErrors,
  ...hygieneErrors,
  ...mirrorErrors,
  ...runtimeEnvErrors,
  ...secretFileReferenceErrors,
];

if (errors.length > 0) {
  console.error(`Docs content validation failed with ${errors.length} issue(s):`);
  for (const error of errors) console.error(`- ${error}`);
  process.exit(1);
}

console.log(
  `Docs content validation passed (${pageUrls.size} pages, ${countRelativeLinks()} internal links, ${countIcons()} icons).`
);
if (contentWarnings.length > 0) {
  console.warn(`Docs content validation warnings (${contentWarnings.length}):`);
  for (const warning of contentWarnings) console.warn(`- ${warning}`);
}

function listFiles(dir, predicate) {
  const files = [];
  for (const name of fs.readdirSync(dir)) {
    const fullPath = path.join(dir, name);
    const stat = fs.statSync(fullPath);
    if (stat.isDirectory()) {
      files.push(...listFiles(fullPath, predicate));
    } else if (predicate(fullPath)) {
      files.push(fullPath);
    }
  }
  return files;
}

function pageUrlForFile(file) {
  let slug = path.relative(contentRoot, file).replaceAll(path.sep, '/').replace(/\.mdx$/, '');
  const parts = slug.split('/');
  const localePrefix = parts[0] === 'en' ? '/en/' : '/';
  if (parts[0] === 'en') parts.shift();

  if (parts.length === 1 && parts[0] === 'index') return localePrefix;
  if (parts.at(-1) === 'index') parts.pop();
  return `${localePrefix}${parts.join('/')}/`;
}

function validateInternalLinks() {
  const errors = [];
  const linkPattern = /\[[^\]]+\]\(([^)]+)\)|href=(['"])([^'"]+)\2|^\s*link:\s*['"]?([^'"\s]+)['"]?\s*$/gm;

  for (const file of mdxFiles) {
    const text = fs.readFileSync(file, 'utf8');
    const baseUrl = pageUrlForFile(file);
    let match;

    while ((match = linkPattern.exec(text))) {
      const raw = (match[1] || match[3] || match[4] || '').trim();
      if (!shouldValidateLink(raw)) continue;

      const targetWithoutHash = raw.split('#')[0];
      if (!targetWithoutHash) continue;

      const resolved = normalizePagePath(
        targetWithoutHash.startsWith('/')
          ? targetWithoutHash
          : new URL(targetWithoutHash, `https://docs.local${baseUrl}`).pathname
      );

      if (!pageUrls.has(resolved)) {
        errors.push(
          `${relative(file)}:${lineForIndex(text, match.index)} links to ${JSON.stringify(raw)}, which resolves to missing page ${resolved}`
        );
      }
    }
  }

  return errors;
}

function shouldValidateLink(raw) {
  if (!raw) return false;
  if (raw.startsWith('#') || raw.startsWith('{')) return false;
  if (/^[a-z][a-z0-9+.-]*:/i.test(raw)) return false;
  return raw.startsWith('.') || raw.startsWith('/') || /^[a-z0-9]/i.test(raw);
}

function normalizePagePath(urlPath) {
  if (urlPath.endsWith('.html')) return urlPath;
  return urlPath.endsWith('/') ? urlPath : `${urlPath}/`;
}

function validateIcons() {
  const availableIcons = loadStarlightIcons();
  const errors = [];
  const iconPattern = /icon=(['"])([^'"]+)\1|^\s*icon:\s*['"]?([A-Za-z0-9_.$:-]+)['"]?\s*$/gm;

  for (const file of docsFiles) {
    const text = fs.readFileSync(file, 'utf8');
    let match;

    while ((match = iconPattern.exec(text))) {
      const icon = match[2] || match[3];
      if (!availableIcons.has(icon)) {
        errors.push(`${relative(file)}:${lineForIndex(text, match.index)} uses unknown Starlight icon ${JSON.stringify(icon)}`);
      }
    }
  }

  return errors;
}

function validateContentHygiene() {
  const errors = [];
  const forbiddenPatterns = [
    { pattern: /\b(?:TODO|FIXME)\b/i, reason: 'contains TODO/FIXME marker' },
    { pattern: /(?:^|[(/])deployment\//, reason: 'references removed deployment/ route' },
    { pattern: /(?:^|[(/])guides\//, reason: 'references removed guides/ route' },
    { pattern: /develop\/develop\//, reason: 'contains duplicated develop/develop path' },
    { pattern: /用户使用指南|管理员操作手册|媒体 Provider 配方/, reason: 'uses retired Chinese documentation title' },
    { pattern: /User Guide|Administration Runbook|Media Provider Recipes/, reason: 'uses retired English documentation title' },
  ];

  for (const file of docsFiles) {
    const text = fs.readFileSync(file, 'utf8');
    for (const { pattern, reason } of forbiddenPatterns) {
      const match = pattern.exec(text);
      if (match) {
        errors.push(`${relative(file)}:${lineForIndex(text, match.index)} ${reason}`);
      }
    }
  }

  return errors;
}

function validateEnglishMirrors() {
  const errors = [];
  const files = new Set(mdxFiles.map((file) => path.relative(contentRoot, file).replaceAll(path.sep, '/')));

  for (const file of files) {
    if (file.startsWith('en/')) continue;
    const mirror = `en/${file}`;
    if (!files.has(mirror)) {
      errors.push(`${file} has no English mirror at ${mirror}`);
    }
  }

  return errors;
}

function validateRuntimeEnvironmentVariables() {
  const errors = [];
  const configSourcePath = path.join(workspaceRoot, 'synctv-core/src/config.rs');
  const envReferencePath = path.join(contentRoot, 'en/reference/environment-variables.mdx');
  const configSource = fs.readFileSync(configSourcePath, 'utf8');
  const envReference = fs.readFileSync(envReferencePath, 'utf8');

  const runtimeEnvVars = new Set(extractRuntimeEnvVars(configSource));
  runtimeEnvVars.add('SYNCTV_CONFIG_PATH');

  const documentedEnvVars = new Set(
    [...envReference.matchAll(/`(SYNCTV_[A-Z0-9_]+)`/g)].map((match) => match[1])
  );

  const undocumented = [...runtimeEnvVars].filter((name) => !documentedEnvVars.has(name)).sort();
  const stale = [...documentedEnvVars].filter((name) => !runtimeEnvVars.has(name)).sort();

  if (undocumented.length > 0) {
    errors.push(
      `reference/environment-variables.mdx is missing runtime SYNCTV_ variable(s): ${undocumented.join(', ')}`
    );
  }
  if (stale.length > 0) {
    errors.push(
      `reference/environment-variables.mdx documents unsupported runtime SYNCTV_ variable(s): ${stale.join(', ')}`
    );
  }

  return errors;
}

function extractRuntimeEnvVars(configSource) {
  const start = configSource.indexOf('fn apply_env_overrides_with');
  if (start < 0) {
    throw new Error('Could not find Config::apply_env_overrides_with in synctv-core/src/config.rs');
  }

  const end = configSource.indexOf('fn resolve_owned_local_paths', start);
  if (end < 0) {
    throw new Error('Could not find end of Config::apply_env_overrides_with in synctv-core/src/config.rs');
  }

  const section = configSource.slice(start, end);
  return [...new Set([...section.matchAll(/"(SYNCTV_[A-Z0-9_]+)"/g)].map((match) => match[1]))];
}

function validateSecretFileReferences() {
  const errors = [];
  const configSourcePath = path.join(workspaceRoot, 'synctv-core/src/config.rs');
  const docsPath = path.join(contentRoot, 'en/configuration/how-configuration-works.mdx');
  const configSource = fs.readFileSync(configSourcePath, 'utf8');
  const docs = fs.readFileSync(docsPath, 'utf8');

  const supportedFields = extractSupportedSecretFileFields(configSource);
  const documentedFields = extractDocumentedSecretFileFields(docs);

  const undocumented = [...supportedFields].filter((field) => !documentedFields.has(field)).sort();
  const stale = [...documentedFields].filter((field) => !supportedFields.has(field)).sort();

  if (undocumented.length > 0) {
    errors.push(
      `configuration/how-configuration-works.mdx is missing secret-file field(s): ${undocumented.join(', ')}`
    );
  }
  if (stale.length > 0) {
    errors.push(
      `configuration/how-configuration-works.mdx documents unsupported secret-file field(s): ${stale.join(', ')}`
    );
  }

  return errors;
}

function extractSupportedSecretFileFields(configSource) {
  const start = configSource.indexOf('fn supports_secret_file_reference');
  if (start < 0) {
    throw new Error('Could not find supports_secret_file_reference in synctv-core/src/config.rs');
  }

  const end = configSource.indexOf('fn resolve_secret_file_references_in_json_value', start);
  if (end < 0) {
    throw new Error('Could not find end of supports_secret_file_reference in synctv-core/src/config.rs');
  }

  const section = configSource.slice(start, end);
  return new Set(
    [...section.matchAll(/"([a-z][a-z0-9_]*(?:\.[a-z0-9_]+)+)"/g)].map((match) => match[1])
  );
}

function extractDocumentedSecretFileFields(docs) {
  const start = docs.indexOf('Common fields that support file references include:');
  if (start < 0) {
    throw new Error('Could not find secret-file field list in how-configuration-works.mdx');
  }

  const end = docs.indexOf('Relative `*_file` paths', start);
  if (end < 0) {
    throw new Error('Could not find end of secret-file field list in how-configuration-works.mdx');
  }

  const section = docs.slice(start, end);
  return new Set([...section.matchAll(/`([a-z][a-z0-9_]*(?:\.[a-z0-9_]+)+)`/g)].map((match) => match[1]));
}

function collectContentWarnings() {
  const warnings = [];
  const warningPatterns = [
    { pattern: /^##\s+相关页面\s*$/gm, label: 'uses generic “相关页面” heading' },
    { pattern: /^##\s+Related Pages\s*$/gm, label: 'uses generic “Related Pages” heading' },
  ];

  for (const file of mdxFiles) {
    const text = fs.readFileSync(file, 'utf8');
    for (const { pattern, label } of warningPatterns) {
      let match;
      while ((match = pattern.exec(text))) {
        warnings.push(`${relative(file)}:${lineForIndex(text, match.index)} ${label}`);
      }
    }
  }

  return warnings;
}

function loadStarlightIcons() {
  const iconFiles = [
    path.join(starlightRoot, 'components-internals/Icons.ts'),
    path.join(starlightRoot, 'user-components/file-tree-icons.ts'),
  ];
  const icons = new Set();
  const iconKeyPattern = /^\s*(?:'([^']+)'|([A-Za-z0-9_.$-]+))\s*:/gm;

  for (const file of iconFiles) {
    const text = fs.readFileSync(file, 'utf8');
    let match;
    while ((match = iconKeyPattern.exec(text))) {
      icons.add(match[1] || match[2]);
    }
  }

  return icons;
}

function countRelativeLinks() {
  let count = 0;
  const linkPattern = /\[[^\]]+\]\(([^)]+)\)|href=(['"])([^'"]+)\2|^\s*link:\s*['"]?([^'"\s]+)['"]?\s*$/gm;
  for (const file of mdxFiles) {
    const text = fs.readFileSync(file, 'utf8');
    let match;
    while ((match = linkPattern.exec(text))) {
      if (shouldValidateLink((match[1] || match[3] || match[4] || '').trim())) count++;
    }
  }
  return count;
}

function countIcons() {
  let count = 0;
  const iconPattern = /icon=(['"])([^'"]+)\1|^\s*icon:\s*['"]?([A-Za-z0-9_.$:-]+)['"]?\s*$/gm;
  for (const file of docsFiles) {
    const text = fs.readFileSync(file, 'utf8');
    while (iconPattern.exec(text)) count++;
  }
  return count;
}

function lineForIndex(text, index) {
  return text.slice(0, index).split('\n').length;
}

function relative(file) {
  return path.relative(docsRoot, file).replaceAll(path.sep, '/');
}
