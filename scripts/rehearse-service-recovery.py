"""Own a synthetic local PostgreSQL cluster; never load dotenv or production URLs."""
import argparse,json,os,socket,subprocess,time,urllib.error,urllib.request
from pathlib import Path
import tempfile

def port():
    with socket.socket()as sock:sock.bind(('127.0.0.1',0));return sock.getsockname()[1]
def run(args,env):
    # Windows daemons inherit pipe handles; a file prevents communicate() waiting
    # forever for a background PostgreSQL process to close the parent's pipe.
    with tempfile.TemporaryFile()as output:
        result=subprocess.run([str(v)for v in args],env=env,stdout=output,stderr=output,timeout=45)
    if result.returncode:raise RuntimeError('SYNTHETIC_COMMAND_FAILED')
def status(port,path):
    try:
        with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(f'http://127.0.0.1:{port}{path}',timeout=1)as response:return response.status,response.read(128).decode()
    except urllib.error.HTTPError as error:return error.code,''
    except (OSError,urllib.error.URLError):return None,''
def expect(process,port,path,expected):
    deadline=time.monotonic()+30
    while time.monotonic()<deadline:
        if process.poll()is not None:raise RuntimeError('FIXTURE_EXITED')
        if status(port,path)[0]==expected:return
        time.sleep(.1)
    raise RuntimeError('EXPECTED_HEALTH_TRANSITION_MISSING')
def main():
    parser=argparse.ArgumentParser();parser.add_argument('--pg-bin',required=True);parser.add_argument('--fixture-exe',required=True);parser.add_argument('--work-root',required=True);args=parser.parse_args()
    root=Path(args.work_root).resolve();root.mkdir(parents=True,exist_ok=True)
    work=Path(tempfile.mkdtemp(prefix='resilience-fixture-',dir=root));data=work/'data'
    pg=Path(args.pg_bin).resolve();suffix='.exe'if os.name=='nt'else''
    def tool(name):return pg/(name+suffix)
    # Inherit OS runtime settings only, never application or PostgreSQL credentials.
    env={key:value for key,value in os.environ.items()if not(key.upper().startswith(('KABIPAY_','PG','POSTGRES_'))or key.upper()in {'DATABASE_URL','NODE_OPTIONS'})}
    database_port=port();http_port=port();running=False;process=None
    def start():run([tool('pg_ctl'),'-D',data,'-l',work/'postgres.log','-o',f'-h 127.0.0.1 -p {database_port}','-w','start'],env)
    def stop():run([tool('pg_ctl'),'-D',data,'-m','fast','-w','stop'],env)
    try:
        run([tool('initdb'),'-D',data,'-U','fixture_owner','-A','trust','--no-locale','-E','UTF8'],env)
        running=True;start()
        run([tool('psql'),'-X','--no-password','-h','127.0.0.1','-p',database_port,'-U','fixture_owner','-d','postgres','-v','ON_ERROR_STOP=1','-c','CREATE DATABASE resilience_fixture'],env)
        stop();running=False
        env.update(KABIPAY_TEST_DATABASE_URL=f'postgres://fixture_owner@127.0.0.1:{database_port}/resilience_fixture?sslmode=disable',KABIPAY_TEST_PORT=str(http_port),KABIPAY_DEPENDENCY_RETRY_INITIAL_MS='100',KABIPAY_DEPENDENCY_RETRY_MAX_MS='500',KABIPAY_DEPENDENCY_PROBE_INTERVAL_MS='250',KABIPAY_DEPENDENCY_TIMEOUT_MS='1000')
        with open(work/'fixture.log','w',encoding='utf-8')as log:
            process=subprocess.Popen([str(Path(args.fixture_exe).resolve())],env=env,stdout=log,stderr=log)
            expect(process,http_port,'/livez',200);expect(process,http_port,'/readyz',503)
            for cycle in range(2):
                running=True;start();expect(process,http_port,'/readyz',200);expect(process,http_port,'/initialized',200)
                if status(http_port,'/initialized')[1]!='initialized-once':raise RuntimeError('INITIALIZATION_CHANGED')
                stop();running=False;expect(process,http_port,'/readyz',503);expect(process,http_port,'/livez',200);expect(process,http_port,'/initialized',503)
            running=True;start();expect(process,http_port,'/readyz',200)
            print(json.dumps(dict(completed=True,startupDatabaseAbsent=True,recoveryCycles=2,sameProcess=True,readinessGated=True,workDirectory=str(work),productionConnections=False)))
    finally:
        try:
            if process is not None and process.poll()is None:
                process.terminate()
                try:process.wait(timeout=10)
                except subprocess.TimeoutExpired:process.kill();process.wait(timeout=10)
        finally:
            if running and (data/'postmaster.pid').exists():stop()
if __name__=='__main__':main()
