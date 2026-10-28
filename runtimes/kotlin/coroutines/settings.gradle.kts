// SPDX-License-Identifier: Apache-2.0
// The opt-in coroutines layer of the Kotlin runtime. It builds against the
// Kotlin layer's artifact (io.github.pboachie.tungsten:tungsten-runtime-kotlin)
// and the Java runtime's; for a local build, install both first (see ../README.md).
rootProject.name = "tungsten-runtime-kotlin-coroutines"

dependencyResolutionManagement {
    repositories {
        mavenLocal()
        mavenCentral()
    }
}
