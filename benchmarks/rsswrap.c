/* Runs a command and prints its peak resident set size (KiB) to stderr as the last line.
 * A separate tiny process is used because ru_maxrss of a child includes the memory the *parent*
 * had when it forked, which would hide the real difference between the benchmarks. */
#include <stdio.h>
#include <stdlib.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>

int main(int argc, char** argv) {
    if (argc < 2) return 2;
    pid_t pid = fork();
    if (pid == 0) {
        execvp(argv[1], argv + 1);
        perror("exec");
        _exit(127);
    }
    int status = 0;
    struct rusage ru;
    wait4(pid, &status, 0, &ru);
    fflush(stdout);
    fprintf(stderr, "\n%ld\n", ru.ru_maxrss);
    return WIFEXITED(status) ? WEXITSTATUS(status) : 1;
}
