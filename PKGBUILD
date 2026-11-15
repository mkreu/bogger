pkgname=bogger
pkgver=0.1.0
pkgrel=1
pkgdesc="Manual Borg backup helper for ashell"
arch=('x86_64')
license=('unknown')
depends=('borg' 'glibc' 'polkit' 'snapper' 'systemd')
makedepends=('cargo')
install='bogger.install'
options=(!debug)
source=()
sha256sums=()

build() {
  export CARGO_TARGET_DIR="${startdir}/target"
  cd "${startdir}"
  cargo build --release --locked
}

check() {
  export CARGO_TARGET_DIR="${startdir}/target"
  cd "${startdir}"
  cargo test --locked
}

package() {
  install -Dm755 "${startdir}/target/release/${pkgname}" "${pkgdir}/usr/bin/${pkgname}"
  install -Dm644 "${startdir}/contrib/config.toml.example" \
    "${pkgdir}/usr/share/doc/${pkgname}/config.toml.example"
  install -Dm644 "${startdir}/contrib/bogger.tmpfiles" \
    "${pkgdir}/usr/lib/tmpfiles.d/bogger.conf"

  install -Dm644 "${startdir}/contrib/bogger-backup.service" \
    "${pkgdir}/usr/lib/systemd/system/bogger-backup.service"
  sed -i 's#/usr/local/bin/bogger#/usr/bin/bogger#' \
    "${pkgdir}/usr/lib/systemd/system/bogger-backup.service"
}
