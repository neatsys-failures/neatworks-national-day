## NeoBFT: Accelerating Byzantine Fault Tolerance Using Authenticated In-Network Ordering

The evaluation of NeoBFT consists of three parts: 
* AOM micro-benchmarks (section 6.1, figure 4-6)
* four replica performance benchmarks (section 6.2, 6.4 and 6.5, figure 7, 9 and 10)
* AWS deployment (section 6.3, figure 8).

The first two parts should be conducted on a hardware-accessible cluster.
For part one, you will need a Tofino1 switch, a Xillinx AU50 FPGA and a server machine.
For part two, you will need the switch and the FPGA same as part 1, and five server machines, and the first four of which must have identical specification.

For part three, you will need an AWS account (and pay for the bill after evaluation).

For all three parts, you will also need a development environment that builds artifacts, controls the evaluation runs and collect results.
Refer to the [preparation guide](./prepare.md) for detail setup.

During the evaluation, you will only need to issue commands from switch, the first server machine and development environment.

# Collect Data for Part 1

Open three terminals, the first two on switch and the third on the first server.

In the first terminal, start switchd for HMAC program 

```
switch:~$ $SDE/run_switchd.sh -p neo_hmac_bench
```

You may be prompted to enter password for sudo access. 
Wait until the switchd prompt `bfshell>` is shown.

In the second terminal, set up switch program

```
switch:~$ cd neo-switch
switch:~/neo-switch$ ./setup_hmac.sh
```

Start packet generation script

```
switch:~/neo-switch$ ./pktgen_hmac.sh
```

In the third terminal, capture packets (assuming network interface named `ens1f0`)

```
server1:~$ sudo tcpdump -i ens1f0 -e | grep -e "00:00:00:" > extracted.log
```

In the second terminal, start packet generation session

```
PD(neo_fpga)[0]>>> pktgen.app_enable(1)
```

After ~10 seconds, stop packet generation

```
PD(neo_fpga)[0]>>> pktgen.app_disable(1)
```

Enter `ctrl-d` to exit session.

In the third terminal, `ctrl-c` to stop packet capturing.

In the first terminal, `ctrl-\` to stop switchd.

To collect data for PKEY variant, repeat the same process while replacing `hmac` with `fpga`, i.e. `neo_hmac_bench` to `neo_fpga_bench`, `./setup_hmac.sh` to `./setup_fpga.sh`, `./pktgen_hmac.sh` to `./pktgen_fpga.sh`.

# Collect Data for Part 2

Open three terminals, the first two on switch and the third on dev.

In the first terminal, start switchd for HMAC program

```
switch:~$ $SDE/run_switchd.sh -p neo_hmac
```

You may be prompted to enter password for sudo access. 
Wait until the switchd prompt `bfshell>` is shown.

In the second terminal, set up switch program

```
switch:~$ cd neo-switch
switch:~/neo-switch$ ./setup_hmac.sh
```

In the third terminal, reload servers and run benchmarks

```
dev:~$ cd neobft-artifact
dev:~/neobft-artifact$ cargo -q run -p reload
    Finished release [optimized] target(s) in 0.16s
* server started on server1
dev:~/neobft-artifact$ cargo -q run -p control -- hmac
```

The results will be saved to `saved-hmac.csv`.
The complete run takes minutes.
If the evaluating is crashed or killed manually, it will resume in the next run.
Just make sure to reload because running it again.

After running, in the first terminal enter `ctrl-\` to stop switchd.

Then repeat the process with `hmac` replaced to `fpga`, i.e. `neo_hmac` to `neo_fpga`, `./setup_hmac.sh` to `./setup_fpga.sh`, and `hmac` to `fpga` in the control script command.
The results are saved to `saved-fpga.csv`.

# Collect Data for Part 3

Open one terminal on dev.

Create AWS cluster

```
dev:~$ cd neobft-artifact
dev:~/neobft-artifact$ terraform -chdir=scripts/aws apply
```

Enter "yes" when prompted.

Initialize the cluster

```
dev:~/neobft-artifact$ cargo -q run -p neo-aws -- hmac
```

Reload servers and run control script

```
dev:~/neobft-artifact$ cargo -q run -p reload --features aws
    Finished release [optimized] target(s) in 0.07s
* server started on ip-x-x-x-x.ap-east-1.compute.amazonaws.com
dev:~/neobft-artifact$ cargo -q run -p control --features aws -- aws-hmac
```

The results will be saved to `saved-aws-hmac.csv`.

Repeat the process with `hmac` replaced with `fpga`.

After running, destroy AWS cluster

```
dev:~/neobft-artifact$ terraform -chdir=scripts/aws destroy
```

Enter "yes" when prompted.

# Visualization

Collect all result files into `data` directory and refer to the notebooks.

# Disclaimer

This is a complete overhaul of the original version.
The exact code the produces camera-ready results can be found in `master-legacy` branch.

Noticeable implementation differences:
* Replace public key cryptography library `secp256k1` to `k256`
* Remove cryptography offloading and worker threads
* Apply the same pace-based adaptive batching strategy to all protocols universally
  * The Zyzzyva implementation is currently buggy under the new strategy, but its faulty variant seems normal

These changes help produce more realistic results, better explore protocol's best and typical latency, and reduce resource utilization.
