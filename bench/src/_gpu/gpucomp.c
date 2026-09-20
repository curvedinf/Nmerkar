/* gpucomp.c — shared Vulkan compute launcher for the non-Nmerkar benchmark
 * languages (bench/SPEC.md). Device selection mirrors the uf compiler
 * runtime: auto = most free VRAM (VK_EXT_memory_budget, discrete preferred),
 * or pinned via "vkN". Kernels are .comp shaders compiled by run.py with
 * glslc; this library only loads and LAUNCHES them.
 *
 *   int  gpu_init(const char* device);
 *   int  gpu_run(const char* kernel, uint64_t n,
 *                const double* A, const double* B, double* R,
 *                int64_t n0, int64_t n1, int64_t n2, int64_t n3,
 *                double s, int64_t rev);
 * Buffers: A/B inputs (B may be NULL), R output. n = work item count.
 * Push constants follow the uf shader library layout (see compute.rs).
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <vulkan/vulkan.h>

#define WG 256
struct PC { long n0, n1, n2, n3; double s; long rev; };

static VkInstance inst;
static VkPhysicalDevice pd;
static VkDevice dev;
static VkQueue q;
static uint32_t qf;
static VkCommandPool pool;
static VkCommandBuffer cb;
static VkPipelineLayout play;
static VkDescriptorSetLayout dsl;
static VkDescriptorPool dpool;
static VkBuffer dummy;
static VkDeviceMemory dummy_mem;
static int ready, broken;

typedef struct { char name[32]; VkPipeline pipe; } KPipe;
static KPipe kpipe[32];
static int nkpipe;

static int kfind(const char* name){ for(int i=0;i<nkpipe;i++) if(!strcmp(kpipe[i].name,name)) return i; return -1; }

static uint64_t free_mem(VkPhysicalDevice p, int have_budget, int* discrete){
  VkPhysicalDeviceProperties pr; vkGetPhysicalDeviceProperties(p,&pr);
  *discrete = pr.deviceType==VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU;
  if(have_budget){
    VkPhysicalDeviceMemoryProperties2 mp; memset(&mp,0,sizeof mp); mp.sType=VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MEMORY_PROPERTIES_2;
    VkPhysicalDeviceMemoryBudgetPropertiesEXT bp; memset(&bp,0,sizeof bp); bp.sType=VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MEMORY_BUDGET_PROPERTIES_EXT;
    mp.pNext=&bp; vkGetPhysicalDeviceMemoryProperties2(p,&mp);
    uint64_t f=0;
    for(uint32_t i=0;i<mp.memoryProperties.memoryHeapCount;i++)
      if(mp.memoryProperties.memoryHeaps[i].flags&VK_MEMORY_HEAP_DEVICE_LOCAL_BIT){ uint64_t b=bp.heapBudget[i],u=bp.heapUsage[i]; f += b>u ? b-u : 0; }
    if(f) return f;
  }
  VkPhysicalDeviceMemoryProperties mp; vkGetPhysicalDeviceMemoryProperties(p,&mp);
  uint64_t t=0;
  for(uint32_t i=0;i<mp.memoryHeapCount;i++) if(mp.memoryHeaps[i].flags&VK_MEMORY_HEAP_DEVICE_LOCAL_BIT) t+=mp.memoryHeaps[i].size;
  return t;
}

static void load_kernel(const char* name, const char* spv_path){
  if(nkpipe>=32) return;
  FILE* f=fopen(spv_path,"rb"); if(!f) return;
  fseek(f,0,SEEK_END); long sz=ftell(f); fseek(f,0,SEEK_SET);
  uint32_t* code=(uint32_t*)malloc(sz?sz:1);
  if(fread(code,1,sz,f)!=(size_t)sz){ fclose(f); free(code); return; }
  fclose(f);
  VkShaderModuleCreateInfo sm; memset(&sm,0,sizeof sm); sm.sType=VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO; sm.codeSize=sz; sm.pCode=code;
  VkShaderModule mod;
  if(vkCreateShaderModule(dev,&sm,0,&mod)!=VK_SUCCESS){ free(code); return; }
  VkComputePipelineCreateInfo cp; memset(&cp,0,sizeof cp); cp.sType=VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO;
  cp.stage.sType=VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO; cp.stage.stage=VK_SHADER_STAGE_COMPUTE_BIT; cp.stage.module=mod; cp.stage.pName="main"; cp.layout=play;
  VkPipeline pipe;
  if(vkCreateComputePipelines(dev,0,1,&cp,0,&pipe)==VK_SUCCESS){
    snprintf(kpipe[nkpipe].name,sizeof kpipe[0].name,"%s",name);
    kpipe[nkpipe].pipe=pipe; nkpipe++;
  }
  vkDestroyShaderModule(dev,mod,0); free(code);
}

int gpu_init(const char* device){
  if(ready||broken) return ready?0:-1;
  VkApplicationInfo app; memset(&app,0,sizeof app); app.sType=VK_STRUCTURE_TYPE_APPLICATION_INFO; app.pApplicationName="ufbench"; app.apiVersion=VK_API_VERSION_1_1;
  VkInstanceCreateInfo ci; memset(&ci,0,sizeof ci); ci.sType=VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO; ci.pApplicationInfo=&app;
  if(vkCreateInstance(&ci,0,&inst)!=VK_SUCCESS){ broken=1; return -1; }
  uint32_t nd=0; vkEnumeratePhysicalDevices(inst,&nd,0);
  if(!nd){ broken=1; return -1; }
  VkPhysicalDevice* pds=(VkPhysicalDevice*)malloc(nd*sizeof(VkPhysicalDevice)); vkEnumeratePhysicalDevices(inst,&nd,pds);
  int sel=-1;
  if(device[0]=='v'&&device[1]=='k'&&device[2]>='0'&&device[2]<='9'){
    long want=atol(device+2);
    if((uint32_t)want>=nd){ free(pds); broken=1; return -1; }
    sel=(int)want;
  } else {
    /* hardware first (discrete > integrated > software), most free VRAM
       within the class — llvmpipe advertises RAM-sized budgets and its
       software float64 pipelines are unusably slow */
    uint64_t best=0; int bcls=-1;
    for(uint32_t i=0;i<nd;i++){
      uint32_t nec=0; vkEnumerateDeviceExtensionProperties(pds[i],0,&nec,0);
      VkExtensionProperties* ex=(VkExtensionProperties*)malloc(nec*sizeof(VkExtensionProperties)); vkEnumerateDeviceExtensionProperties(pds[i],0,&nec,ex);
      int hb=0; for(uint32_t k=0;k<nec;k++) if(!strcmp(ex[k].extensionName,"VK_EXT_memory_budget")) hb=1;
      free(ex);
      VkPhysicalDeviceProperties pr; vkGetPhysicalDeviceProperties(pds[i],&pr);
      int cls = pr.deviceType==VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU?2
              : pr.deviceType==VK_PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU?1 : 0;
      int disc; uint64_t fm=free_mem(pds[i],hb,&disc);
      if(cls>bcls||(cls==bcls&&fm>best)){ best=fm; bcls=cls; sel=(int)i; }
    }
  }
  if(sel<0){ free(pds); broken=1; return -1; }
  pd=pds[sel]; free(pds);
  uint32_t nq=0; vkGetPhysicalDeviceQueueFamilyProperties(pd,&nq,0);
  VkQueueFamilyProperties* qfam=(VkQueueFamilyProperties*)malloc(nq*sizeof(VkQueueFamilyProperties)); vkGetPhysicalDeviceQueueFamilyProperties(pd,&nq,qfam);
  qf=UINT32_MAX; for(uint32_t i=0;i<nq;i++) if(qfam[i].queueFlags&VK_QUEUE_COMPUTE_BIT){ qf=i; break; }
  free(qfam);
  if(qf==UINT32_MAX){ broken=1; return -1; }
  float pri=1.0f;
  VkDeviceQueueCreateInfo qci; memset(&qci,0,sizeof qci); qci.sType=VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO; qci.queueFamilyIndex=qf; qci.queueCount=1; qci.pQueuePriorities=&pri;
  const char* devexts[1]; uint32_t ndevext=0;
  uint32_t nec=0; vkEnumerateDeviceExtensionProperties(pd,0,&nec,0);
  VkExtensionProperties* ex=(VkExtensionProperties*)malloc(nec*sizeof(VkExtensionProperties)); vkEnumerateDeviceExtensionProperties(pd,0,&nec,ex);
  for(uint32_t k=0;k<nec;k++) if(!strcmp(ex[k].extensionName,"VK_EXT_memory_budget")) devexts[ndevext++]=ex[k].extensionName;
  free(ex);
  VkDeviceCreateInfo dci; memset(&dci,0,sizeof dci); dci.sType=VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO; dci.queueCreateInfoCount=1; dci.pQueueCreateInfos=&qci; dci.enabledExtensionCount=ndevext; dci.ppEnabledExtensionNames=devexts;
  if(vkCreateDevice(pd,&dci,0,&dev)!=VK_SUCCESS){ broken=1; return -1; }
  vkGetDeviceQueue(dev,qf,0,&q);
  VkCommandPoolCreateInfo pci; memset(&pci,0,sizeof pci); pci.sType=VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO; pci.queueFamilyIndex=qf; pci.flags=VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT;
  if(vkCreateCommandPool(dev,&pci,0,&pool)!=VK_SUCCESS){ broken=1; return -1; }
  VkCommandBufferAllocateInfo cai; memset(&cai,0,sizeof cai); cai.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO; cai.commandPool=pool; cai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cai.commandBufferCount=1;
  if(vkAllocateCommandBuffers(dev,&cai,&cb)!=VK_SUCCESS){ broken=1; return -1; }
  VkDescriptorSetLayoutBinding lb[3];
  for(int i=0;i<3;i++){ lb[i].binding=(uint32_t)i; lb[i].descriptorType=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER; lb[i].descriptorCount=1; lb[i].stageFlags=VK_SHADER_STAGE_COMPUTE_BIT; lb[i].pImmutableSamplers=0; }
  VkDescriptorSetLayoutCreateInfo dli; memset(&dli,0,sizeof dli); dli.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO; dli.bindingCount=3; dli.pBindings=lb;
  vkCreateDescriptorSetLayout(dev,&dli,0,&dsl);
  VkPushConstantRange pr; pr.stageFlags=VK_SHADER_STAGE_COMPUTE_BIT; pr.offset=0; pr.size=sizeof(struct PC);
  VkPipelineLayoutCreateInfo pli; memset(&pli,0,sizeof pli); pli.sType=VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO; pli.setLayoutCount=1; pli.pSetLayouts=&dsl; pli.pushConstantRangeCount=1; pli.pPushConstantRanges=&pr;
  vkCreatePipelineLayout(dev,&pli,0,&play);
  VkDescriptorPoolSize ps; ps.type=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER; ps.descriptorCount=64;
  VkDescriptorPoolCreateInfo dpi; memset(&dpi,0,sizeof dpi); dpi.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO; dpi.flags=VK_DESCRIPTOR_POOL_CREATE_FREE_DESCRIPTOR_SET_BIT; dpi.maxSets=16; dpi.poolSizeCount=1; dpi.pPoolSizes=&ps;
  vkCreateDescriptorPool(dev,&dpi,0,&dpool);
  VkBufferCreateInfo dbi; memset(&dbi,0,sizeof dbi); dbi.sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO; dbi.size=16; dbi.usage=VK_BUFFER_USAGE_STORAGE_BUFFER_BIT;
  vkCreateBuffer(dev,&dbi,0,&dummy);
  VkMemoryRequirements mr; vkGetBufferMemoryRequirements(dev,dummy,&mr);
  VkMemoryAllocateInfo mai; memset(&mai,0,sizeof mai); mai.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO; mai.allocationSize=mr.size;
  VkPhysicalDeviceMemoryProperties mp; vkGetPhysicalDeviceMemoryProperties(pd,&mp);
  for(uint32_t i=0;i<mp.memoryTypeCount;i++) if(mp.memoryTypes[i].propertyFlags&VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT){ mai.memoryTypeIndex=i; break; }
  vkAllocateMemory(dev,&mai,0,&dummy_mem); vkBindBufferMemory(dev,dummy,dummy_mem,0);
  /* load every .spv next to SPV_DIR (or cwd) */
  const char* dirs[2]={getenv("UF_SPV_DIR"),"."};
  const char* names[32]={"eadd","esub","emul","ediv","badd","bsub","bmul","bdiv","matmul","matvec","rsum","rmin","rmax","esqrt","transpose","bs"};
  for(int d=0;dirs[d];d++) for(int i=0;i<16;i++){
    char path[512]; snprintf(path,sizeof path,"%s/%s.spv",dirs[d],names[i]);
    load_kernel(names[i],path);
  }
  ready=1; return 0;
}

typedef struct { VkBuffer buf; VkDeviceMemory mem; void* mapped; } GBuf;
static int mkbuf(size_t sz,const void* init,GBuf*o){
  memset(o,0,sizeof *o);
  VkBufferCreateInfo bi; memset(&bi,0,sizeof bi); bi.sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO; bi.size=sz?sz:16; bi.usage=VK_BUFFER_USAGE_STORAGE_BUFFER_BIT;
  if(vkCreateBuffer(dev,&bi,0,&o->buf)!=VK_SUCCESS) return 0;
  VkMemoryRequirements mr; vkGetBufferMemoryRequirements(dev,o->buf,&mr);
  VkMemoryAllocateInfo ai; memset(&ai,0,sizeof ai); ai.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO; ai.allocationSize=mr.size;
  VkPhysicalDeviceMemoryProperties mp; vkGetPhysicalDeviceMemoryProperties(pd,&mp);
  int found=0;
  for(uint32_t i=0;i<mp.memoryTypeCount;i++) if((mp.memoryTypes[i].propertyFlags&VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT)&&(mr.memoryTypeBits&(1u<<i))){ ai.memoryTypeIndex=i; found=1; break; }
  if(!found||vkAllocateMemory(dev,&ai,0,&o->mem)!=VK_SUCCESS){ vkDestroyBuffer(dev,o->buf,0); return 0; }
  vkBindBufferMemory(dev,o->buf,o->mem,0);
  if(vkMapMemory(dev,o->mem,0,sz?sz:16,0,&o->mapped)!=VK_SUCCESS){ vkFreeMemory(dev,o->mem,0); vkDestroyBuffer(dev,o->buf,0); return 0; }
  if(init&&sz) memcpy(o->mapped,init,sz);
  return 1;
}
static void rmbuf(GBuf*b){ if(b->mem){ vkUnmapMemory(dev,b->mem); vkFreeMemory(dev,b->mem,0); } if(b->buf) vkDestroyBuffer(dev,b->buf,0); memset(b,0,sizeof *b); }

int gpu_run_sized(const char* kernel, uint64_t n, const double* A, size_t asz, const double* B, size_t bsz, double* R, size_t rsz,
            int64_t n0, int64_t n1, int64_t n2, int64_t n3, double s, int64_t rev){
  if(broken||!ready){ if(!broken) gpu_init("auto"); if(!ready) return -1; }
  int k=kfind(kernel); if(k<0) return -1;
  GBuf ba,bb,br; memset(&ba,0,sizeof ba); memset(&bb,0,sizeof bb); memset(&br,0,sizeof br);
  if(!mkbuf(asz,A,&ba)) return -1;
  if(B&&!mkbuf(bsz,B,&bb)){ rmbuf(&ba); return -1; }
  if(!mkbuf(rsz,NULL,&br)){ rmbuf(&ba); rmbuf(&bb); return -1; }
  VkDescriptorSetAllocateInfo dai; memset(&dai,0,sizeof dai); dai.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO; dai.descriptorPool=dpool; dai.descriptorSetCount=1; dai.pSetLayouts=&dsl;
  VkDescriptorSet ds;
  if(vkAllocateDescriptorSets(dev,&dai,&ds)!=VK_SUCCESS){ rmbuf(&ba); rmbuf(&bb); rmbuf(&br); return -1; }
  VkWriteDescriptorSet w[3]; VkDescriptorBufferInfo bi[3];
  memset(w,0,sizeof w); memset(bi,0,sizeof bi);
  bi[0].buffer=ba.buf; bi[0].range=VK_WHOLE_SIZE;
  bi[1].buffer=B?bb.buf:dummy; bi[1].range=VK_WHOLE_SIZE;
  bi[2].buffer=br.buf; bi[2].range=VK_WHOLE_SIZE;
  for(int i=0;i<3;i++){ w[i].sType=VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET; w[i].dstSet=ds; w[i].dstBinding=(uint32_t)i; w[i].descriptorCount=1; w[i].descriptorType=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER; w[i].pBufferInfo=&bi[i]; }
  vkUpdateDescriptorSets(dev,3,w,0,0);
  struct PC pc; pc.n0=n0; pc.n1=n1; pc.n2=n2; pc.n3=n3; pc.s=s; pc.rev=rev;
  VkCommandBufferBeginInfo cbi; memset(&cbi,0,sizeof cbi); cbi.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO;
  vkBeginCommandBuffer(cb,&cbi);
  vkCmdBindPipeline(cb,VK_PIPELINE_BIND_POINT_COMPUTE,kpipe[k].pipe);
  vkCmdPushConstants(cb,play,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof pc,&pc);
  vkCmdBindDescriptorSets(cb,VK_PIPELINE_BIND_POINT_COMPUTE,play,0,1,&ds,0,0);
  uint64_t groups=(n+WG-1)/WG; if(!groups)groups=1;
  vkCmdDispatch(cb,(uint32_t)(groups>0x7fffffff?0x7fffffff:groups),1,1);
  vkEndCommandBuffer(cb);
  VkSubmitInfo si; memset(&si,0,sizeof si); si.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO; si.commandBufferCount=1; si.pCommandBuffers=&cb;
  VkFence fence; VkFenceCreateInfo fc; memset(&fc,0,sizeof fc); fc.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO;
  vkCreateFence(dev,&fc,0,&fence);
  int ok=vkQueueSubmit(q,1,&si,fence)==VK_SUCCESS && vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX)==VK_SUCCESS;
  vkDestroyFence(dev,fence,0);
  if(ok&&R&&rsz) memcpy(R,br.mapped,rsz);
  vkFreeDescriptorSets(dev,dpool,1,&ds);
  rmbuf(&ba); rmbuf(&bb); rmbuf(&br);
  return ok?0:-1;
}

int gpu_run(const char* kernel, uint64_t n, const double* A, const double* B, double* R,
            int64_t n0, int64_t n1, int64_t n2, int64_t n3, double s, int64_t rev){
  return gpu_run_sized(kernel, n, A, A?n*8:0, B, B?n*8:0, R, R?n*8:0, n0,n1,n2,n3,s,rev);
}
