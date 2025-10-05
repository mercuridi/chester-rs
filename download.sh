#!/bin/bash

# Path to your SQLite database
DB_PATH="database/metadata.sqlite3"

# Output directory
OUTPUT_DIR="audio"
mkdir -p "$OUTPUT_DIR"

# Number of parallel downloads
PARALLEL_JOBS=8  # Adjust based on your CPU/network

# Function to download a single video ID
download_video() {
    ID="$1"
    FILE="$OUTPUT_DIR/$ID.mp3"

    if [[ -f "$FILE" ]]; then
        echo "Skipping $ID, already downloaded."
        return
    fi

    echo "Downloading $ID..."
    yt-dlp -t mp3 -o "$OUTPUT_DIR/%(id)s.%(ext)s" --no-playlist --no-progress "https://www.youtube.com/watch?v=$ID"
    
    if [[ $? -ne 0 ]]; then
        echo "Failed to download $ID"
    fi
}

export -f download_video
export OUTPUT_DIR

# Fetch all video IDs and feed them to xargs for parallel execution
sqlite3 "$DB_PATH" "SELECT id FROM tracks;" | xargs -n 1 -P "$PARALLEL_JOBS" -I {} bash -c 'download_video "$@"' _ {}
