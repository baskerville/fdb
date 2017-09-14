# Bootstrap

	fdb -z

# Examples

ZSH configuration:

	precmd() {
		[ "$PWD" -ef "$HOME" ] || fdb -a "$PWD"
	}

Shell function for jumping to most frecent directory that matches the patterns given as arguments:

	z() {
		local dir=$(fdb -q "$@" | head -n 1)
		[ -z "$dir" ] && return 1
		cd "$dir" || fdb -d "$dir"
	}


# Frecency

The *frecency* is computed as:

	        H
	—————————————————
	 0.25 + 3·10⁻⁶·A

Where *H* is the number of hits, and *A* the age.

# Environment variables

- `FDB_DB_PATH`.
- `FDB_HISTORY_SIZE`.
