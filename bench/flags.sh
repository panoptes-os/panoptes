# Sourced. lean_flags SET fills F with extra Chromium flags and sets APP=1
# when the page should open with --app. Each set adds to the one before it:
#   base    E1 flags only, page in a normal tab
#   app     --app window: no tab strip, no omnibox
#   quiet   background services off (the Playwright/Puppeteer style list)
#   proc    GPU and network service in the browser process, no spare renderer
#   lowend  --enable-low-end-device-mode
#   lite    V8 lite mode (no optimizing compiler)
#   noiso   site isolation off, one renderer process (unsafe with logins)
declare -A LEVEL=([base]=0 [app]=1 [quiet]=2 [proc]=3 [lowend]=4 [lite]=5
  [noiso]=6)
lean_flags() {
  local l=${LEVEL[$1]:?unknown flag set $1} dis=() en=()
  F=() APP=$((l >= 1))
  if ((l >= 2)); then
    F+=(--disable-background-networking --disable-component-update
      --disable-sync --disable-extensions --disable-default-apps
      --disable-breakpad --disable-domain-reliability
      --disable-client-side-phishing-detection
      --disable-component-extensions-with-background-pages
      --metrics-recording-only --no-pings --mute-audio)
    dis+=(Translate OptimizationHints MediaRouter DialMediaRouteProvider
      CertificateTransparencyComponentUpdater AutofillServerCommunication
      LensOverlay)
  fi
  if ((l >= 3)); then
    F+=(--in-process-gpu)
    en+=(NetworkServiceInProcess2 NetworkServiceInProcess)
    dis+=(SpareRendererForSitePerProcess)
  fi
  ((l >= 4)) && F+=(--enable-low-end-device-mode)
  ((l >= 5)) && F+=(--js-flags=--lite-mode)
  ((l >= 6)) && F+=(--disable-site-isolation-trials --renderer-process-limit=1)
  # Chromium keeps only the last --enable-features and --disable-features.
  ((${#dis[@]})) && F+=("--disable-features=$(IFS=,; echo "${dis[*]}")")
  ((${#en[@]})) && F+=("--enable-features=$(IFS=,; echo "${en[*]}")")
  return 0
}
