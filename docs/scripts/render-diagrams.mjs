import { execFile } from 'node:child_process';
import { mkdir } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const execFileAsync = promisify(execFile);
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const mmdc = path.join(root, 'node_modules', '.bin', process.platform === 'win32' ? 'mmdc.cmd' : 'mmdc');
const sourceDir = path.join(root, 'src', 'diagrams');
const outputDir = path.join(root, 'src', 'assets', 'diagrams');
const lightConfig = path.join(root, 'scripts', 'mermaid-light-config.json');
const darkConfig = path.join(root, 'scripts', 'mermaid-dark-config.json');
const puppeteerConfig = path.join(root, 'scripts', 'mermaid-puppeteer-config.json');

const diagrams = [
  'architecture',
  'security-auth-boundary',
  'production-minimal',
  'kubernetes-topology',
  'cluster-runtime',
  'livestream-pipeline',
];

await mkdir(outputDir, { recursive: true });

for (const diagram of diagrams) {
  for (const [theme, config] of [
    ['light', lightConfig],
    ['dark', darkConfig],
  ]) {
    await execFileAsync(
      mmdc,
      [
        '--input',
        path.join(sourceDir, `${diagram}.mmd`),
        '--output',
        path.join(outputDir, `${diagram}-${theme}.svg`),
        '--configFile',
        config,
        '--puppeteerConfigFile',
        puppeteerConfig,
        '--backgroundColor',
        'transparent',
      ],
      { cwd: root },
    );
  }
}
