// SPDX-License-Identifier: Apache-2.0
// The Kotlin layer of the tungsten runtime. It builds against the Java
// runtime's artifact (io.github.pboachie.tungsten:tungsten-runtime); for a
// local build, install it first with `mvn -q install` in ../java.
rootProject.name = "tungsten-runtime-kotlin"

dependencyResolutionManagement {
    repositories {
        mavenLocal()
        mavenCentral()
    }
}
