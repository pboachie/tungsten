// SPDX-License-Identifier: Apache-2.0
// tungsten-runtime-kotlin-coroutines: the opt-in suspend and Flow layer for
// generated Kotlin SDKs built with the `coroutines` target option. It is a
// separate artifact so that tungsten-runtime-kotlin (the default, blocking
// surface) keeps depending only on the Java runtime and kotlin-stdlib; only an
// SDK that opts in pulls in kotlinx-coroutines-core. Built with --no-daemon and
// -Pkotlin.compiler.execution.strategy=in-process in automation.
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    kotlin("jvm") version "2.4.21"
    `java-library`
    `maven-publish`
}

group = "io.github.pboachie.tungsten"
version = "0.1.6"

dependencies {
    api("io.github.pboachie.tungsten:tungsten-runtime-kotlin:0.1.6")
    api("io.github.pboachie.tungsten:tungsten-runtime:0.1.6")
    api("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.10.2")
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
    from("../README.md", "../LICENSE") { into("META-INF") }
    manifest {
        attributes("Automatic-Module-Name" to "io.github.pboachie.tungsten.runtime.kotlin.coroutines")
    }
}

publishing {
    publications {
        create<MavenPublication>("maven") {
            artifactId = "tungsten-runtime-kotlin-coroutines"
            from(components["java"])
            pom {
                name.set("tungsten runtime for Kotlin coroutines")
                description.set("The opt-in suspend and Flow layer of the Kotlin SDKs tungsten generates: cancellable suspend calls and cold Flows of pages and server-sent events over the blocking Kotlin runtime.")
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
