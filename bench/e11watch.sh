#!/usr/bin/env bash
# E11's watcher, run as root. While CURFILE names an agent N, move each new
# renderer process in CGROUP/browser into CGROUP/agent-N. A process seen
# while CURFILE holds 0 stays where it is, so renderers that existed before
# an agent, or appear between agents, are not filed under the next agent. A
# renderer forks from the zygote and may still show the zygote's command
# line for a moment, so zygote-typed processes are checked again on every
# pass; any other process is read once and remembered.
# usage: e11watch.sh CGROUP CURFILE
cg=$1 cur=$2
declare -A done_pid
while :; do
  a=$(< "$cur")
  while read -r p; do
    [ -n "${done_pid[$p]:-}" ] && continue
    if [ "$a" = 0 ]; then done_pid[$p]=1; continue; fi
    t=$(tr '\0 ' '\n\n' < "/proc/$p/cmdline" 2> /dev/null | grep -m1 -- '^--type=')
    case $t in
      --type=renderer) echo "$p" > "$cg/agent-$a/cgroup.procs" ;;
      --type=zygote) ;;
      *) done_pid[$p]=1 ;;
    esac
  done < "$cg/browser/cgroup.procs"
  sleep 0.005
done
