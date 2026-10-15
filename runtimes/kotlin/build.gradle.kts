// SPDX-License-Identifier: Apache-2.0
// tungsten-runtime-kotlin: the thin Kotlin layer the generated Kotlin SDKs
// call (identity, Patch, JSON helpers, sequences, Result extensions). All
// transport and semantics are the Java runtime's; the only dependencies are
// that runtime and kotlin-stdlib. Built with --no-daemon and
// -Pkotlin.compiler.execution.strategy=in-process in automation.
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    kotlin("jvm") version "2.4.21"
    `java-library`
    `maven-publish`
}

group = "io.github.pboachie.tungsten"
version = "0.1.0"

dependencies {
    api("io.github.pboachie.tungsten:tungsten-runtime:0.1.0")
}

java {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
    withSourcesJar()
}

kotlin {
    explicitApi()
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
        allWarningsAsErrors.set(true)
    }
}

tasks.jar {
    manifest {
        attributes("Automatic-Module-Name" to "io.github.pboachie.tungsten.runtime.kotlin")
    }
}

publishing {
    publications {
        create<MavenPublication>("maven") {
            artifactId = "tungsten-runtime-kotlin"
            from(components["java"])
            pom {
                name.set("tungsten runtime for Kotlin")
                description.set("The Kotlin layer of the SDKs tungsten generates, over the Java runtime: the Kotlin identity, Patch, JSON helpers, Sequence adapters and Result extensions.")
                url.set("https://github.com/pboachie/tungsten")
                licenses {
                    license {
                        name.set("Apache-2.0")
                        url.set("https://www.apache.org/licenses/LICENSE-2.0")
                    }
                }
                scm {
                    url.set("https://github.com/pboachie/tungsten")
                }
            }
        }
    }
}
