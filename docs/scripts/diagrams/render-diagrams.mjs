import { execFile } from 'node:child_process';
import { mkdir, readdir } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const execFileAsync = promisify(execFile);
const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(scriptDir, '..', '..');
const mmdc = path.join(root, 'node_modules', '.bin', process.platform === 'win32' ? 'mmdc.cmd' : 'mmdc');
const sourceDir = path.join(root, 'src', 'diagrams');
const outputDir = path.join(root, 'src', 'assets', 'diagrams');
const lightConfig = path.join(scriptDir, 'mermaid-light-config.json');
const darkConfig = path.join(scriptDir, 'mermaid-dark-config.json');
const puppeteerConfig = path.join(scriptDir, 'mermaid-puppeteer-config.json');

await mkdir(outputDir, { recursive: true });

const diagrams = (await readdir(sourceDir, { withFileTypes: true }))
  .filter((entry) => entry.isFile() && entry.name.endsWith('.mmd'))
  .map((entry) => path.basename(entry.name, '.mmd'))
  .sort((left, right) => left.localeCompare(right));

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
