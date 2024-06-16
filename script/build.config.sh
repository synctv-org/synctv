function parseDepArgs() {
    for i in "$@"; do
        case ${i,,} in
        --version=*)
            version="${i#*=}"
            shift
            ;;
        --skip-init-web)
            skip_init_web="true"
            shift
            ;;
        --web-version=*)
            web_version="${i#*=}"
            shift
            ;;
        *)
            return 1
            ;;
        esac
    done
}

function printDepHelp() {
    echo -e "${COLOR_YELLOW}--version=${COLOR_RESET} set build version (default: dev)"
    echo -e "${COLOR_YELLOW}--web-version=${COLOR_RESET} set web dependency version (default: VERSION)"
    echo -e "${COLOR_YELLOW}--skip-init-web${COLOR_RESET}"
}

function printDepEnvHelp() {
    echo -e "${COLOR_GREEN}VERSION${COLOR_RESET} (default: dev)"
    echo -e "${COLOR_GREEN}WEB_VERSION${COLOR_RESET} set web dependency version (default: VERSION)"
    echo -e "${COLOR_GREEN}SKIP_INIT_WEB${COLOR_RESET}"
}

function initDepPlatforms() {
    if ! isCGOEnabled; then
        deleteFromAllowedPlatforms "linux/mips,linux/mips64,linux/mips64le,linux/mipsle,linux/ppc64"
        deleteFromAllowedPlatforms "windows/386,windows/arm"
    fi
}

function initDep() {
    setDefault "version" "dev"
    version="$(echo "$version" | sed 's/ //g' | sed 's/"//g' | sed 's/\n//g')"
    if [[ "${version}" != "dev" ]] && [[ ! "${version}" =~ ^v?[0-9]+\.[0-9]+\.[0-9]+(-beta.*|-rc.*|-alpha.*)?$ ]]; then
        echo "version format error: ${version}"
        return 1
    fi
    setDefault "web_version" "${version}"
    setDefault "skip_init_web" ""

    echo -e "${COLOR_BLUE}version:${COLOR_RESET} ${COLOR_GREEN}${version}${COLOR_RESET}"

    addLDFLAGS "-X 'github.com/synctv-org/synctv/internal/version.Version=${version}'"
    setDefault "web_version" "${version}"
    addLDFLAGS "-X 'github.com/synctv-org/synctv/internal/version.WebVersion=${web_version}'"

    local git_commit
    git_commit="$(git log --pretty=format:"%h" -1)" || git_commit="unknown"
    addLDFLAGS "-X 'github.com/synctv-org/synctv/internal/version.GitCommit=${git_commit}'"

    if [[ -z "${skip_init_web}" ]] && [[ -n "${web_version}" ]]; then
        downloadAndUnzip "https://github.com/synctv-org/synctv-web/releases/download/${web_version}/dist.tar.gz" "${source_dir}/public/dist"
    fi

    addTags "jsoniter"
}
