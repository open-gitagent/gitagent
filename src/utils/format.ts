import chalk from 'chalk';

export function success(msg: string): void {
  console.log(chalk.green('✓') + ' ' + msg);
}

export function error(msg: string): void {
  console.log(chalk.red('✗') + ' ' + msg);
}

export function warn(msg: string): void {
  console.log(chalk.yellow('!') + ' ' + msg);
}

export function info(msg: string): void {
  console.log(chalk.blue('i') + ' ' + msg);
}

export function heading(msg: string): void {
  console.log('\n' + chalk.bold(msg));
}

export function label(key: string, value: string): void {
  console.log(`  ${chalk.gray(key + ':')} ${value}`);
}

export function divider(): void {
  console.log(chalk.gray('─'.repeat(60)));
}
