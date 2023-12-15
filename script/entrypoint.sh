#!/bin/bash

chown -R ${PUID}:${PGID} ~/.synctv

umask ${UMASK}

exec su-exec ${PUID}:${PGID} synctv --env-no-prefix $@
