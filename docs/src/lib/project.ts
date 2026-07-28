const defaultRepository = 'synctv-org/synctv';
const defaultAppVersion = '1.0.1-rc.1';

function readEnv(name: string): string | undefined {
  const value = process.env[name]?.trim();
  return value ? value : undefined;
}

function stripSuffix(value: string, suffix: string): string {
  return value.endsWith(suffix) ? value.slice(0, -suffix.length) : value;
}

function normalizeBasePath(value: string | undefined): string {
  if (!value || value === '/') {
    return '/';
  }

  return `/${stripSuffix(value.replace(/^\/+/, ''), '/')}/`;
}

function repositoryName(repository: string): string {
  return repository.split('/').at(-1) || repository;
}

function dockerHubImage(repository: string): string {
  const username = readEnv('SYNCTV_DOCS_DOCKERHUB_USERNAME') || readEnv('DOCKERHUB_USERNAME');
  const configuredRepository =
    readEnv('SYNCTV_DOCS_DOCKERHUB_REPOSITORY') || readEnv('DOCKERHUB_REPOSITORY');

  if (configuredRepository) {
    return configuredRepository.includes('/') || !username
      ? configuredRepository
      : `${username}/${configuredRepository}`;
  }

  return username ? `${username}/${repositoryName(repository)}` : 'synctvorg/synctv';
}

export const docsSite = readEnv('SYNCTV_DOCS_SITE') || 'https://docs.syncs.tv';
export const docsBase = normalizeBasePath(readEnv('SYNCTV_DOCS_BASE'));
export const githubRepository =
  readEnv('SYNCTV_DOCS_GITHUB_REPOSITORY') || readEnv('GITHUB_REPOSITORY') || defaultRepository;
export const githubBranch =
  readEnv('SYNCTV_DOCS_GITHUB_BRANCH') ||
  readEnv('GITHUB_REF_NAME') ||
  readEnv('GITHUB_HEAD_REF') ||
  'main';
export const githubUrl = `https://github.com/${githubRepository}`;
export const githubCloneUrl = `${githubUrl}.git`;
export const githubEditUrl = `${githubUrl}/edit/${githubBranch}/docs/`;

export const dockerImage = readEnv('SYNCTV_DOCS_DOCKER_IMAGE') || dockerHubImage(githubRepository);
export const dockerImageTag = readEnv('SYNCTV_DOCS_IMAGE_TAG') || defaultAppVersion;
export const dockerImageReference = `${dockerImage}:${dockerImageTag}`;

export const helmChartName = readEnv('SYNCTV_DOCS_HELM_CHART_NAME') || 'synctv';
export const helmChartVersion = readEnv('SYNCTV_DOCS_HELM_CHART_VERSION') || defaultAppVersion;
export const helmOciRepository =
  readEnv('SYNCTV_DOCS_HELM_OCI_REPOSITORY') ||
  readEnv('HELM_OCI_REPOSITORY') ||
  `ghcr.io/${githubRepository}/charts`;
export const helmOciChartReference = `oci://${helmOciRepository}/${helmChartName}`;
