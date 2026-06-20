pkgname=borg-reminder
pkgver=0.1.0
pkgrel=1
pkgdesc="Manual Borg backup helper for ashell"
arch=('x86_64')
license=('unknown')
depends=('borg' 'glibc' 'polkit' 'snapper' 'systemd')
makedepends=('cargo' 'git')
install='borg-reminder.install'
options=(!debug)
source=()
sha256sums=()

_srcdir_name="${pkgname}-${pkgver}"

prepare() {
  local buildsrc="${srcdir}/${_srcdir_name}"
  local paths=()

  rm -rf "${buildsrc}"

  while IFS= read -r -d '' path; do
    paths+=("${path}")
  done < <(git -C "${startdir}" ls-files -z --cached --modified --others --exclude-standard -- \
    . \
    ':(exclude)pkg/**')

  mkdir -p "${buildsrc}"

  for path in "${paths[@]}"; do
    cp --parents -a "${startdir}/${path}" "${buildsrc}"
  done
}

build() {
  local buildsrc="${srcdir}/${_srcdir_name}"

  cd "${buildsrc}"
  export CARGO_TARGET_DIR="${srcdir}/target"
  cargo build --release --locked
}

check() {
  local buildsrc="${srcdir}/${_srcdir_name}"

  cd "${buildsrc}"
  export CARGO_TARGET_DIR="${srcdir}/target"
  cargo test --locked
}

package() {
  local buildsrc="${srcdir}/${_srcdir_name}"
  local service_path="${srcdir}/borg-reminder-backup.service"

  install -Dm755 "${srcdir}/target/release/${pkgname}" "${pkgdir}/usr/bin/${pkgname}"
  install -Dm644 "${buildsrc}/contrib/config.toml.example" \
    "${pkgdir}/usr/share/doc/${pkgname}/config.toml.example"

  sed 's#/usr/local/bin/borg-reminder#/usr/bin/borg-reminder#' \
    "${buildsrc}/contrib/borg-reminder-backup.service" > "${service_path}"
  install -Dm644 "${service_path}" \
    "${pkgdir}/usr/lib/systemd/system/borg-reminder-backup.service"
}
