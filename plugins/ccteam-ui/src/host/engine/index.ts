/**
 * The engine face: locate → install → supervise, plus the credential
 * bootstrap that makes a local daemon zero-configuration.
 *
 * Barrel only — every rule lives in the module that owns it.
 */
export {
  canonicalBinaryPath,
  resolveCcteamHome,
  canonicalInstallDir,
  canonicalPath,
  daemonLogPath,
  defaultEnvironment,
  dialableUrl,
  discoverDaemonUrl,
  endpointPath,
  enginePackageName,
  enginePlatform,
  findOnPath,
  isExecutableFile,
  locateEngine,
  parseVersionOutput,
  processExists,
  readDaemonEndpoint,
  runCommand,
  tailFile,
  webTokenPath,
  ENGINE_PLATFORMS,
  type DaemonEndpoint,
  type EngineEnvironment,
  type EngineLocation,
  type EnginePlatform,
  type RunFn,
  type RunResult,
} from './locate.js'
export {
  classifyDestPath,
  classifyDestination,
  installEngine,
  resolveInstallDir,
  resolvePackageBin,
  type DestVerdict,
  type InstallOutcome,
  type ResolvePackageBin,
} from './install.js'
export {
  EngineSupervisor,
  isLoopbackUrl,
  lastJsonObject,
  type HealthBody,
  type SupervisorOptions,
} from './supervisor.js'
export {
  createEnrollmentBootstrap,
  createTokenBootstrap,
  requestEnrollment,
  type EnrollmentBootstrap,
  type EnrollmentOptions,
  type EnrollmentOutcome,
  type TokenBootstrap,
} from './bootstrap.js'
