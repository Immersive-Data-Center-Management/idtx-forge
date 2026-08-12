/**
* @file version.h
 * @brief Single source of truth for the IDTXForge version.
 *
 **/
#pragma once

#define IDTXFORGE_VERSION_MAJOR 0
#define IDTXFORGE_VERSION_MINOR 0
#define IDTXFORGE_VERSION_PATCH 1

#define IDTXFORGE_STRINGIFY(x)  #x
#define IDTXFORGE_TOSTRING(x)   IDTXFORGE_STRINGIFY(x)

/// Full semantic version string e.g. "0.0.1"
#define IDTXFORGE_VERSION \
IDTXFORGE_TOSTRING(IDTXFORGE_VERSION_MAJOR) "." \
IDTXFORGE_TOSTRING(IDTXFORGE_VERSION_MINOR) "." \
IDTXFORGE_TOSTRING(IDTXFORGE_VERSION_PATCH)

/// Application name + version string for display e.g. "IDTXForge v0.0.1"
#define IDTXFORGE_VERSION_STRING "IDTXForge v" IDTXFORGE_VERSION
