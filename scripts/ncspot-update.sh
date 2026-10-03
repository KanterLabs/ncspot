# Compatibility source path for existing dotfiles.
#
# Resolve the implementation relative to this file, rather than the caller's
# working directory, so `. scripts/ncspot-update.sh` remains valid everywhere.
_ncspot_update_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "$_ncspot_update_dir/resonance-update.sh"
unset _ncspot_update_dir

ncspot-update() {
  resonance-update "$@"
}
