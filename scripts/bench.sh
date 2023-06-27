#!/usr/bin/env bash
set -e

psql -U lemmy -d postgres -c "DROP DATABASE lemmy;"
psql -U lemmy -d postgres -c "CREATE DATABASE lemmy;"

export LEMMY_DATABASE_URL=postgres://lemmy:password@localhost:5432/lemmy
export LEMMY_CONFIG_LOCATION=config/config.hjson

cargo bench
