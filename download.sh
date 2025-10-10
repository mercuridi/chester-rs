#!/bin/bash

# Path to your SQLite database
DB_PATH="database/metadata.sqlite3"

# Output directory
OUTPUT_DIR="audio"
mkdir -p "$OUTPUT_DIR"

# Default: no parallel execution
PARALLEL_MODE=false
PARALLEL_JOBS=8  # Adjust this number if needed

# Parse CLI arguments
for arg in "$@"; do
    case "$arg" in
        -p|--parallel)
            PARALLEL_MODE=true
            ;;
        *)
            echo "Usage: $0 [--parallel|-p]"
            exit 1
            ;;
    esac
done

# Function to download a single video ID
download_video() {
    ID="$1"
    FILE="$OUTPUT_DIR/$ID.mp3"

    if [[ -f "$FILE" ]]; then
        echo "Skipping $ID, already downloaded."
        return
    fi

    echo "Downloading $ID..."
    ./yt-dlp -x --audio-format mp3 --audio-quality 0 \
        --ffmpeg-location "$(which ffmpeg)" \
        -o "$OUTPUT_DIR/%(id)s.%(ext)s" \
        --no-playlist --no-progress \
        "https://www.youtube.com/watch?v=$ID"

    if [[ $? -ne 0 ]]; then
        echo "Failed to download $ID"
    fi
}

# Export the function and output dir for xargs if using parallel
export -f download_video
export OUTPUT_DIR

# Run in parallel or serial mode
if [ "$PARALLEL_MODE" = true ]; then
    echo "Running in parallel mode with $PARALLEL_JOBS jobs..."
    sqlite3 "$DB_PATH" "SELECT id FROM tracks;" | xargs -n 1 -P "$PARALLEL_JOBS" -I {} bash -c 'download_video "$@"' _ {}
else
    echo "Running in serial mode..."
    while IFS= read -r ID; do
        download_video "$ID"
    done < <(sqlite3 "$DB_PATH" "SELECT id FROM tracks;")
fi
