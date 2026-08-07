import { execFile } from 'node:child_process';
import { mkdir, readdir, stat } from 'node:fs/promises';
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
const force = process.argv.includes('--force');

await mkdir(outputDir, { recursive: true });

async function newestMtimeMs(paths) {
  const stats = await Promise.all(paths.map((filePath) => stat(filePath)));
  return Math.max(...stats.map((fileStat) => fileStat.mtimeMs));
}

async function shouldRender(input, output, config) {
  if (force) {
    return true;
  }
  try {
    const [outputStat, newestInputMtime] = await Promise.all([
      stat(output),
      newestMtimeMs([input, config, puppeteerConfig]),
    ]);
    return outputStat.mtimeMs < newestInputMtime;
  } catch (error) {
    if (error.code === 'ENOENT') {
      return true;
    }
    throw error;
  }
}

const diagrams = (await readdir(sourceDir, { withFileTypes: true }))
  .filter((entry) => entry.isFile() && entry.name.endsWith('.mmd'))
  .map((entry) => path.basename(entry.name, '.mmd'))
  .sort((left, right) => left.localeCompare(right));

for (const diagram of diagrams) {
  for (const [theme, config] of [
    ['light', lightConfig],
    ['dark', darkConfig],
  ]) {
    const input = path.join(sourceDir, `${diagram}.mmd`);
    const output = path.join(outputDir, `${diagram}-${theme}.svg`);
    if (!(await shouldRender(input, output, config))) {
      continue;
    }
    await execFileAsync(
      mmdc,
      [
        '--input',
        input,
        '--output',
        output,
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
