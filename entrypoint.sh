#!/bin/sh
set -e

BP="${BASE_PATH:-}"
STATIC="${STATIC_DIR:-/srv/static}"
SENTINEL="/__HAPPYVIEW_BP__"
MARKER="${STATIC}/.base-path-pending"

# Only run replacement once (marker is created during Docker build)
if [ -f "$MARKER" ]; then
    if [ -n "$BP" ]; then
        # Validate: must start with /
        case "$BP" in
            /*) ;;
            *) echo "ERROR: BASE_PATH must start with '/' (got: $BP)" >&2; exit 1 ;;
        esac
        # Strip trailing slash
        BP="${BP%/}"

        # Replace sentinel string in static files with actual base path
        find "${STATIC}" -type f \( -name '*.html' -o -name '*.js' -o -name '*.css' -o -name '*.txt' \) \
            -exec sed -i "s|${SENTINEL}|${BP}|g" {} +
    else
        # No base path: remove sentinel string from static files
        find "${STATIC}" -type f \( -name '*.html' -o -name '*.js' -o -name '*.css' -o -name '*.txt' \) \
            -exec sed -i "s|${SENTINEL}||g" {} +
    fi

    rm "$MARKER"
fi

# The image's other binary runs as itself; everything else is happyview's.
if [ "$1" = "happyview-codemod" ]; then
    shift
    exec happyview-codemod "$@"
fi

exec happyview "$@"
