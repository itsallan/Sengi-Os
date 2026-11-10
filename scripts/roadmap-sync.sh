#!/bin/sh
# Mirror ROADMAP.md to GitHub: one milestone per "## X.Y" heading, one issue
# per checklist line. Safe to run again; existing items are left alone and
# ticked items get their issue closed. Needs gh with a token in GH_TOKEN.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
repo=${GITHUB_REPOSITORY:?set GITHUB_REPOSITORY to owner/name}

milestones=$(gh api "repos/$repo/milestones?state=all&per_page=100" --jq '.[].title')
issues=$(gh issue list -R "$repo" --state all --limit 500 --json number,title,state \
	--jq '.[] | "\(.number)\t\(.state)\t\(.title)"')
tab=$(printf '\t')

release=
while IFS= read -r line; do
	case $line in
	"## "*)
		release=${line#\#\# }
		if ! echo "$milestones" | grep -qxF "$release"; then
			gh api "repos/$repo/milestones" -f title="$release" \
				-f description="Sengi OS $release" >/dev/null
			echo "milestone $release"
		fi
		;;
	"- ["?"] "*)
		[ -n "$release" ] || continue
		title=${line#- \[?\] }
		existing=$(echo "$issues" | awk -F"$tab" -v t="$title" '$3 == t {print $1 " " $2; exit}')
		case $line in "- [x]"*) done=1 ;; *) done=0 ;; esac
		if [ -z "$existing" ]; then
			gh issue create -R "$repo" --title "$title" --milestone "$release" \
				--body "Tracked in ROADMAP.md under release $release." >/dev/null
			echo "issue [$release] $title"
		elif [ $done = 1 ] && [ "${existing#* }" = OPEN ]; then
			gh issue close -R "$repo" "${existing% *}" >/dev/null
			echo "closed [$release] $title"
		fi
		;;
	esac
done <"$root/ROADMAP.md"
