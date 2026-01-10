import { formatBenchUsage, parseBenchCli } from './runner/config.ts';
import { runScenario } from './runner/run.ts';
import { getScenarioById, scenarios } from './scenarios/index.ts';

const command = parseBenchCli(process.argv.slice(2), process.env);

if (command.kind === 'help') {
  if (command.error) console.error(command.error);
  process.stdout.write(formatBenchUsage());
  process.exitCode = command.error ? 1 : 0;
} else if (command.kind === 'list') {
  for (const scenario of scenarios) {
    const req = scenario.requirements ?? {};
    const flags: string[] = [];
    if (req.diskImage === 'required') flags.push('disk-image');
    if (req.webgpu) flags.push('webgpu');
    if (req.opfs) flags.push('opfs');
    if (req.crossOriginIsolated) flags.push('crossOriginIsolated');

    process.stdout.write(
      `${scenario.id}\t${scenario.kind}\t${scenario.name}${flags.length ? `\t[${flags.join(', ')}]` : ''}\n`,
    );
  }
} else {
  const scenario = getScenarioById(command.config.scenarioId);
  if (!scenario) {
    console.error(`Unknown scenario: ${command.config.scenarioId}`);
    process.stdout.write('\n');
    process.stdout.write(formatBenchUsage());
    process.exitCode = 1;
  } else {
    const report = await runScenario(scenario, command.config);
    const detail =
      report.status === 'skipped' && report.skipReason
        ? ` (${report.skipReason})`
        : report.status === 'error' && report.error
          ? ` (${report.error.message})`
          : '';
    process.stdout.write(`\nStatus: ${report.status}${detail}\n`);
    process.stdout.write(`Saved report to ${command.config.outDir}/report.json\n`);
    if (report.status === 'error') process.exitCode = 1;
  }
}
