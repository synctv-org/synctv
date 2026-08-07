import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const docsRoot = path.resolve(fileURLToPath(new URL('..', import.meta.url)));
const contentRoot = path.join(docsRoot, 'src/content/docs');
const starlightRoot = path.join(docsRoot, 'node_modules/@astrojs/starlight');

const mdxFiles = listFiles(contentRoot, (file) => file.endsWith('.mdx'));
const docsFiles = [...mdxFiles, path.join(docsRoot, 'astro.config.mjs')];
const docsContent = new Map(docsFiles.map((file) => [file, fs.readFileSync(file, 'utf8')]));
const pageUrls = new Set(mdxFiles.map(pageUrlForFile));

const { errors: linkErrors, count: relativeLinkCount } = validateInternalLinks();
const { errors: iconErrors, count: iconCount } = validateIcons();
const mirrorErrors = validateEnglishMirrors();
const errors = [
  ...linkErrors,
  ...iconErrors,
  ...mirrorErrors,
];

if (errors.length > 0) {
  console.error(`Docs content validation failed with ${errors.length} issue(s):`);
  for (const error of errors) console.error(`- ${error}`);
  process.exit(1);
}

console.log(
  `Docs content validation passed (${pageUrls.size} pages, ${relativeLinkCount} internal links, ${iconCount} icons).`
);
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
  let count = 0;
  const linkPattern = /\[[^\]]+\]\(([^)]+)\)|href=(['"])([^'"]+)\2|^\s*link:\s*['"]?([^'"\s]+)['"]?\s*$/gm;

  for (const file of mdxFiles) {
    const text = docsContent.get(file);
    const baseUrl = pageUrlForFile(file);
    let match;

    while ((match = linkPattern.exec(text))) {
      const raw = (match[1] || match[3] || match[4] || '').trim();
      if (!shouldValidateLink(raw)) continue;
      count++;

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

  return { errors, count };
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
  let count = 0;
  const iconPattern = /icon=(['"])([^'"]+)\1|^\s*icon:\s*['"]?([A-Za-z0-9_.$:-]+)['"]?\s*$/gm;

  for (const file of docsFiles) {
    const text = docsContent.get(file);
    let match;

    while ((match = iconPattern.exec(text))) {
      count++;
      const icon = match[2] || match[3];
      if (!availableIcons.has(icon)) {
        errors.push(`${relative(file)}:${lineForIndex(text, match.index)} uses unknown Starlight icon ${JSON.stringify(icon)}`);
      }
    }
  }

  return { errors, count };
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

function lineForIndex(text, index) {
  return text.slice(0, index).split('\n').length;
}

function relative(file) {
  return path.relative(docsRoot, file).replaceAll(path.sep, '/');
}
