#!/bin/bash

chown -R ${PUID}:${PGID} /root/.synctv

umask ${UMASK}

export ENV_NO_PREFIX=true

export DATA_DIR=/root/.synctv

exec su-exec ${PUID}:${PGID} synctv $@ --skip-env-flag=false
